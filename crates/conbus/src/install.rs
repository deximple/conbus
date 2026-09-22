//! Registering the hook with the harnesses that will call it.
//!
//! Merging, never overwriting. These files belong to the user and usually
//! already hold their own hooks; the recorder adds itself and leaves everything
//! else byte-for-byte alone.
//!
//! Two vendor facts shape this and both were measured, not assumed. Codex reads
//! `$CODEX_HOME/hooks.json`, **not** `settings.json` — that path appears only in
//! its one-shot import of Claude sessions. And a Codex hook marked
//! `"async": true` is *dropped* by 0.147.0 with a warning, so registering `Stop`
//! that way would silently cost every turn-end record.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

/// Where a vendor keeps its own configuration, which is how `install` knows the
/// vendor is on this machine at all.
///
/// Registering a vendor that is not installed used to create its directories:
/// someone with only Claude Code got `~/.codex`, `~/.grok`, `~/.cursor` and
/// `~/.config/opencode` from conbus, for tools they never had, and uninstall
/// could not tell those directories were ours to remove.
pub fn vendor_home(harness: &str, home: &Path) -> PathBuf {
    match harness {
        "claude_code" => home.join(".claude"),
        "codex" => std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex")),
        "grok" => home.join(".grok"),
        "cursor" => home.join(".cursor"),
        "opencode" => home.join(".config").join("opencode"),
        other => home.join(format!(".{other}")),
    }
}

/// Whether the vendor has ever run here.
pub fn vendor_present(harness: &str, home: &Path) -> bool {
    vendor_home(harness, home).is_dir()
}

/// What a merge would do to one hook event.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Change {
    /// The recorder was not registered for this event and would be.
    Add(String),
    /// The recorder is already registered for this event.
    Keep(String),
}

impl Change {
    pub fn event(&self) -> &str {
        match self {
            Change::Add(event) | Change::Keep(event) => event,
        }
    }
}

/// The Claude Code events the recorder mirrors.
pub const CLAUDE_EVENTS: [&str; 13] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "PermissionRequest",
    "PermissionDenied",
    "Notification",
    "Stop",
    "StopFailure",
    "SubagentStop",
    "SessionEnd",
];

/// Grok's documented Claude-compatible events. `StopFailure` is the quota
/// substitute: remaining JSON does not exist, but `error=rate_limit` does.
pub const GROK_EVENTS: [&str; 9] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "Stop",
    "StopFailure",
    "SubagentStop",
    "SessionEnd",
];

/// The Codex events the recorder mirrors. Its hook surface is smaller: no
/// failure, batch, denial, or notification events exist there at all.
pub const CODEX_EVENTS: [&str; 7] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PermissionRequest",
    "Stop",
    "SessionEnd",
];

/// Add the recorder's hook to a settings document, returning the new document
/// and what changed. The input is never mutated.
pub fn merge(existing: &Value, events: &[&str], command: &str) -> (Value, Vec<Change>) {
    let mut document = existing.as_object().cloned().unwrap_or_default();
    let mut hooks = document
        .get("hooks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut changes = Vec::new();

    for event in events {
        let mut blocks = hooks
            .get(*event)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if blocks.iter().any(|block| mentions(block, command)) {
            changes.push(Change::Keep((*event).to_string()));
            continue;
        }
        blocks.push(json!({ "hooks": [{ "type": "command", "command": command }] }));
        hooks.insert((*event).to_string(), Value::Array(blocks));
        changes.push(Change::Add((*event).to_string()));
    }

    document.insert("hooks".to_string(), Value::Object(hooks));
    (Value::Object(document), changes)
}

/// Whether a registered command runs this recorder's hook, wherever the binary
/// that registered it lives.
///
/// Matched on the program's file name, not on our own path: a registration
/// left pointing at an old checkout's build is still ours to report in
/// `doctor` and still ours to take out in `uninstall`.
pub fn is_conbus_hook(command: &str) -> bool {
    command
        .split_once(" hook")
        .map(|(program, _)| program.trim())
        .and_then(|program| Path::new(program).file_name())
        .is_some_and(|name| name == "conbus")
}

/// Whether a registration block already runs this command.
fn mentions(block: &Value, command: &str) -> bool {
    block
        .get("hooks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|hook| hook.get("command")?.as_str())
        .any(|existing| existing == command)
}

/// Cursor's `~/.cursor/hooks.json` is flat `{ command }`, not Claude's nested
/// `{ hooks: [{ type, command }] }`. `sessionStart` is the official briefing
/// channel (`additional_context`). `beforeSubmitPrompt` mints. `afterAgentResponse`
/// is the official last-bubble channel. `sessionEnd` is the official
/// `error_message` / `final_status` quota-code channel.
pub const CURSOR_EVENTS: [&str; 5] = [
    "sessionStart",
    "beforeSubmitPrompt",
    "stop",
    "sessionEnd",
    "afterAgentResponse",
];

/// Cursor native hooks file. Distinct from `~/.cursor/hooks/hooks.json`,
/// which is a Claude-schema copy some plugins write and Cursor does not run.
pub fn cursor_hooks_path(home: &Path) -> PathBuf {
    home.join(".cursor").join("hooks.json")
}

fn mentions_cursor(block: &Value, command: &str) -> bool {
    block.get("command").and_then(Value::as_str) == Some(command) || mentions(block, command)
}

/// Merge into Cursor's native hooks.json. Existing entries stay first.
pub fn merge_cursor(existing: &Value, events: &[&str], command: &str) -> (Value, Vec<Change>) {
    let mut document = existing.as_object().cloned().unwrap_or_default();
    document.entry("version".to_string()).or_insert(json!(1));
    let mut hooks = document
        .get("hooks")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut changes = Vec::new();

    for event in events {
        let mut blocks = hooks
            .get(*event)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if blocks.iter().any(|block| mentions_cursor(block, command)) {
            changes.push(Change::Keep((*event).to_string()));
            continue;
        }
        blocks.push(json!({ "command": command }));
        hooks.insert((*event).to_string(), Value::Array(blocks));
        changes.push(Change::Add((*event).to_string()));
    }

    document.insert("hooks".to_string(), Value::Object(hooks));
    (Value::Object(document), changes)
}

fn cursor_event_mentions_conbus(value: &Value, event: &str) -> bool {
    value
        .get("hooks")
        .and_then(|hooks| hooks.get(event))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .any(|block| {
            block
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|cmd| cmd.contains("hook") && cmd.contains("conbus"))
        })
}

pub fn cursor_hooks_registered(home: &Path) -> bool {
    let Ok(text) = fs::read_to_string(cursor_hooks_path(home)) else {
        return false;
    };
    let Ok(value) = serde_json::from_str::<Value>(&text) else {
        return false;
    };
    CURSOR_EVENTS
        .iter()
        .all(|event| cursor_event_mentions_conbus(&value, event))
}

/// Register native Cursor events on hooks.json. Merge only.
/// `true` when the file changed.
pub fn apply_cursor_hooks(binary: &Path, home: &Path) -> io::Result<bool> {
    let path = cursor_hooks_path(home);
    let command = format!("{} hook --harness cursor", binary.display());
    let existing = match fs::read_to_string(&path) {
        Ok(text) => serde_json::from_str(&text).map_err(io::Error::other)?,
        Err(err) if err.kind() == io::ErrorKind::NotFound => empty_document(),
        Err(err) => return Err(err),
    };
    let (document, changes) = merge_cursor(&existing, &CURSOR_EVENTS, &command);
    if changes.iter().all(|c| matches!(c, Change::Keep(_))) {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    if path.exists() {
        fs::copy(&path, path.with_extension("json.conbus-backup"))?;
    }
    let text = serde_json::to_string_pretty(&document).map_err(io::Error::other)?;
    write_atomically(&path, &format!("{text}\n"))?;
    Ok(true)
}

/// A `Map` is what both vendors' settings files are at the top level.
pub fn empty_document() -> Value {
    Value::Object(Map::new())
}

/// One harness's registration file.
pub struct Target {
    pub harness: &'static str,
    pub settings: PathBuf,
    pub events: &'static [&'static str],
    pub command: String,
}

/// Where each harness reads its hooks from.
///
/// Codex reads `$CODEX_HOME/hooks.json`, not `settings.json`. That was checked
/// against the running binary: `settings.json` appears in Codex only inside its
/// one-shot import of Claude sessions, so registering there would register
/// nothing.
pub fn targets(binary: &Path, home: &Path) -> Vec<Target> {
    let binary = binary.display();
    let codex_home = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    vec![
        Target {
            harness: "claude_code",
            settings: home.join(".claude").join("settings.json"),
            events: &CLAUDE_EVENTS,
            command: format!("{binary} hook"),
        },
        Target {
            harness: "codex",
            settings: codex_home.join("hooks.json"),
            events: &CODEX_EVENTS,
            command: format!("{binary} hook --harness codex"),
        },
        Target {
            harness: "grok",
            settings: home.join(".grok").join("hooks").join("conbus.json"),
            events: &GROK_EVENTS,
            command: format!("{binary} hook --harness grok"),
        },
    ]
}

/// What one target's registration would become.
pub struct Plan {
    pub target: Target,
    pub document: Value,
    pub changes: Vec<Change>,
    /// Set when the file exists but does not parse. The recorder refuses to
    /// touch it rather than replacing something it cannot read.
    pub unreadable: Option<String>,
    /// The person's existing StatusLine command, saved beside settings so we
    /// observe `rate_limits` without replacing their status bar.
    pub statusline_forward: Option<String>,
}

/// Next to `settings.json`. The original StatusLine command, verbatim.
pub fn statusline_forward_path(settings: &Path) -> PathBuf {
    settings.with_file_name("conbus-statusline-forward")
}

pub fn plan(target: Target) -> io::Result<Plan> {
    let (existing, unreadable) = match fs::read_to_string(&target.settings) {
        Ok(text) => match serde_json::from_str::<Value>(&text) {
            Ok(value) => (value, None),
            Err(err) => (empty_document(), Some(err.to_string())),
        },
        Err(err) if err.kind() == io::ErrorKind::NotFound => (empty_document(), None),
        Err(err) => return Err(err),
    };
    let (mut document, mut changes) = merge(&existing, target.events, &target.command);
    let mut statusline_forward = None;
    if target.harness == "claude_code" {
        let command = format!("{} --statusline", target.command);
        statusline_forward = existing
            .get("statusLine")
            .and_then(|line| line.get("command"))
            .and_then(Value::as_str)
            .filter(|cmd| !cmd.contains("hook --statusline"))
            .map(str::to_string);
        if let Some(change) = merge_statusline(&mut document, &command) {
            changes.push(change);
        }
    }
    Ok(Plan {
        target,
        document,
        changes,
        unreadable,
        statusline_forward,
    })
}

/// Register our StatusLine observer. One slot: wrap, never overwrite.
fn merge_statusline(document: &mut Value, command: &str) -> Option<Change> {
    let existing = document
        .get("statusLine")
        .and_then(|line| line.get("command"))
        .and_then(Value::as_str)
        .map(str::to_string);
    match existing {
        Some(cmd) if cmd.contains("hook --statusline") => Some(Change::Keep("StatusLine".into())),
        Some(_) | None => {
            // Only the command is ours to change. Anything else the person set
            // on their StatusLine — `padding`, say — stays, so uninstalling
            // can put back exactly what was there.
            let mut line = document
                .get("statusLine")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            line.insert("type".into(), json!("command"));
            line.insert("command".into(), json!(command));
            document
                .as_object_mut()?
                .insert("statusLine".into(), Value::Object(line));
            Some(Change::Add("StatusLine".into()))
        }
    }
}

/// Official OpenCode plugin path. Local plugins in this directory load at startup.
pub fn opencode_plugin_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("opencode")
        .join("plugins")
        .join("conbus.js")
}

/// Plugin that forwards official bus events to `conbus hook --harness opencode`.
pub fn opencode_plugin_source(binary: &Path) -> String {
    let bin = binary.display();
    format!(
        r#"// Written by conbus install. Forwards official plugin events; does not call vendor HTTP.
import {{ spawn }} from "node:child_process";

export const ConbusPlugin = async ({{ directory }}) => {{
  const bin = {bin:?};
  const send = (payload) =>
    new Promise((resolve) => {{
      const child = spawn(bin, ["hook", "--harness", "opencode"], {{
        stdio: ["pipe", "pipe", "pipe"],
      }});
      child.on("error", () => resolve());
      child.on("close", () => resolve());
      child.stdin.end(JSON.stringify(payload));
    }});

  return {{
    event: async ({{ event }}) => {{
      const type = event?.type;
      const props = event?.properties ?? {{}};
      const session = props.sessionID ?? props.info?.sessionID ?? props.info?.id;
      if (!type || !session) return;
      if (type === "session.created") {{
        await send({{
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
        }});
        return;
      }}
      if (type === "message.updated" && props.info?.role === "user") {{
        await send({{
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
          message_id: props.info?.id,
          prompt: props.info?.summary?.title ?? "",
        }});
        return;
      }}
      if (type === "message.part.updated" && props.part?.type === "text" && props.part?.text) {{
        await send({{
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: props.part.sessionID ?? session,
          cwd: directory,
          message_id: props.part.messageID,
          prompt: props.part.text,
        }});
        return;
      }}
      if (type === "session.error") {{
        await send({{
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
          error: props.error ?? null,
        }});
      }}
    }},
  }};
}};
"#
    )
}

pub fn opencode_plugin_registered(home: &Path) -> bool {
    std::fs::read_to_string(opencode_plugin_path(home))
        .is_ok_and(|text| text.contains("opencode_plugin") && text.contains("session.error"))
}

/// Write the OpenCode plugin. Idempotent when the command path is unchanged.
pub fn apply_opencode_plugin(binary: &Path, home: &Path) -> io::Result<bool> {
    let path = opencode_plugin_path(home);
    let source = opencode_plugin_source(binary);
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == source) {
        return Ok(false);
    }
    write_atomically(&path, &source)?;
    Ok(true)
}

/// Write the plan, keeping a copy of whatever was there before.
pub fn apply(plan: &Plan) -> io::Result<Option<PathBuf>> {
    let settings = &plan.target.settings;
    if let Some(parent) = settings.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(original) = &plan.statusline_forward {
        write_atomically(&statusline_forward_path(settings), original)?;
    }
    let backup = if settings.exists() {
        let backup = settings.with_extension("json.conbus-backup");
        fs::copy(settings, &backup)?;
        Some(backup)
    } else {
        None
    };
    let text = serde_json::to_string_pretty(&plan.document).map_err(io::Error::other)?;
    write_atomically(settings, &format!("{text}\n"))?;
    Ok(backup)
}

/// Where a stranger's hooks and launchd should point. Not the worktree binary.
pub fn installed_binary(home: &Path) -> PathBuf {
    home.join(".local").join("bin").join("conbus")
}

pub fn launch_agent_path(home: &Path) -> PathBuf {
    home.join("Library")
        .join("LaunchAgents")
        .join("sh.conbus.daemon.plist")
}

/// Replace a file's contents without ever leaving it half written.
///
/// The binary has been placed beside-then-renamed since the two-hour outage;
/// the settings files a person already owned were still truncated in place. A
/// crash, a kill, or a full disk between the truncate and the last byte leaves
/// somebody's `settings.json` unparseable and their CLI will not start — on an
/// install path written for a stranger who does not know a `.conbus-backup`
/// sits beside it. The temp file is made in the target's own directory so the
/// rename stays a rename, the mode the file already had is carried over so
/// replacing an executable does not disarm it, and nothing is left behind when
/// a step fails.
pub fn write_atomically(path: &Path, contents: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("conbus");
    let temp = path.with_file_name(format!("{name}.conbus-next"));
    let written = fs::write(&temp, contents)
        .and_then(|()| keep_mode(path, &temp))
        .and_then(|()| fs::rename(&temp, path));
    if written.is_err() {
        let _ = fs::remove_file(&temp);
    }
    written
}

/// Carry the target's existing mode onto its replacement. A file that is not
/// there yet keeps the default.
fn keep_mode(path: &Path, temp: &Path) -> io::Result<()> {
    let Ok(existing) = fs::metadata(path) else {
        return Ok(());
    };
    fs::set_permissions(temp, existing.permissions())
}

/// Atomic place: write beside, then rename. Overwriting the live inode
/// kills in-flight hooks with exit 137.
pub fn place_binary(current: &Path, dest: &Path) -> io::Result<bool> {
    if same_file(current, dest) {
        return Ok(false);
    }
    if dest.is_file() && fs::read(current)? == fs::read(dest)? {
        return Ok(false);
    }
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    let next = dest.with_file_name("conbus.next");
    fs::copy(current, &next)?;
    fs::rename(next, dest)?;
    Ok(true)
}

/// Whether `current` already is the installed binary, by path or by bytes.
pub fn is_installed(current: &Path, dest: &Path) -> bool {
    same_file(current, dest) || (dest.is_file() && fs::read(current).ok() == fs::read(dest).ok())
}

fn same_file(a: &Path, b: &Path) -> bool {
    match (fs::canonicalize(a), fs::canonicalize(b)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

pub const DAEMON_LABEL: &str = "sh.conbus.daemon";

/// launchd agent that keeps one daemon on the socket.
pub fn daemon_plist(binary: &Path, log: &Path) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{DAEMON_LABEL}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{}</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>{}</string>
  <key>StandardErrorPath</key><string>{}</string>
</dict>
</plist>
"#,
        binary.display(),
        log.display(),
        log.display()
    )
}

/// Write the plist. `load` talks to launchctl; tests leave it false.
pub fn apply_daemon(home: &Path, binary: &Path, log: &Path, load: bool) -> io::Result<bool> {
    let path = launch_agent_path(home);
    let body = daemon_plist(binary, log);
    let unchanged = fs::read_to_string(&path).is_ok_and(|existing| existing == body);
    if !unchanged {
        write_atomically(&path, &body)?;
    }
    if load {
        load_launch_agent(&path)?;
    }
    Ok(!unchanged)
}

fn load_launch_agent(plist: &Path) -> io::Result<()> {
    let uid = unsafe { libc::getuid() };
    let domain = format!("gui/{uid}");
    let target = format!("{domain}/{DAEMON_LABEL}");
    let kick = std::process::Command::new("launchctl")
        .args(["kickstart", "-k", &target])
        .status();
    if kick.is_ok_and(|s| s.success()) {
        return Ok(());
    }
    let _ = std::process::Command::new("launchctl")
        .args(["bootstrap", &domain, &plist.display().to_string()])
        .status();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const CMD: &str = "/opt/conbus hook";

    #[test]
    fn registering_leaves_every_other_setting_exactly_as_it_was() {
        // These files are the user's. Anything the recorder does not need to
        // touch must come back byte-for-byte, including hooks they already run.
        let existing = json!({
            "model": "opus",
            "permissions": { "allow": ["Bash(ls:*)"] },
            "hooks": {
                "Stop": [{ "matcher": "*", "hooks": [{ "type": "command", "command": "their-own-hook" }] }],
                "PreCompact": [{ "hooks": [{ "type": "command", "command": "another" }] }]
            }
        });

        let (merged, _) = merge(&existing, &CLAUDE_EVENTS, CMD);

        assert_eq!(merged["model"], "opus");
        assert_eq!(merged["permissions"], existing["permissions"]);
        assert_eq!(
            merged["hooks"]["PreCompact"],
            existing["hooks"]["PreCompact"]
        );
        assert_eq!(
            merged["hooks"]["Stop"][0], existing["hooks"]["Stop"][0],
            "the user's own Stop hook must survive, and stay first"
        );
        assert!(
            merged["hooks"]["Stop"][1]["hooks"][0]["command"] == CMD,
            "the recorder should be added beside it: {}",
            merged["hooks"]["Stop"]
        );
    }

    #[test]
    fn every_mirrored_event_is_registered() {
        let (merged, changes) = merge(&empty_document(), &CLAUDE_EVENTS, CMD);

        for event in CLAUDE_EVENTS {
            assert_eq!(
                merged["hooks"][event][0]["hooks"][0]["command"], CMD,
                "{event} was not registered"
            );
        }
        assert!(changes.iter().all(|c| matches!(c, Change::Add(_))));
    }

    #[test]
    fn installing_twice_does_not_register_twice() {
        let (once, _) = merge(&empty_document(), &CLAUDE_EVENTS, CMD);

        let (twice, changes) = merge(&once, &CLAUDE_EVENTS, CMD);

        assert_eq!(twice, once, "a second install changed the file");
        assert!(
            changes.iter().all(|c| matches!(c, Change::Keep(_))),
            "{changes:?}"
        );
    }

    #[test]
    fn a_codex_registration_never_marks_a_hook_async() {
        // 0.147.0 prints "skipping async hook" and drops it entirely, so an
        // async `Stop` would silently cost every Codex turn-end record.
        let (merged, _) = merge(&empty_document(), &CODEX_EVENTS, CMD);

        let text = serde_json::to_string(&merged).expect("serialise");
        assert!(!text.contains("async"), "{text}");
    }

    #[test]
    fn a_vendor_is_present_only_when_its_own_directory_is() {
        let home = tempfile::tempdir().expect("tempdir");
        assert!(!vendor_present("grok", home.path()));
        fs::create_dir_all(home.path().join(".grok")).expect("grok");
        assert!(vendor_present("grok", home.path()));
        fs::create_dir_all(home.path().join(".config/opencode")).expect("opencode");
        assert!(vendor_present("opencode", home.path()));
        assert!(!vendor_present("cursor", home.path()));
    }

    #[test]
    fn grok_registers_in_its_own_hooks_dir_not_claude_settings() {
        let home = tempfile::tempdir().expect("tempdir");
        let found = targets(Path::new("/opt/conbus"), home.path())
            .into_iter()
            .find(|t| t.harness == "grok")
            .expect("grok target");
        assert_eq!(
            found.settings,
            home.path().join(".grok").join("hooks").join("conbus.json")
        );
        assert!(found.command.contains("--harness grok"));
        assert!(found.events.contains(&"StopFailure"));
    }

    #[test]
    fn opencode_plugin_is_written_beside_other_local_plugins() {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = Path::new("/opt/conbus");
        assert!(apply_opencode_plugin(bin, home.path()).expect("write"));
        assert!(!apply_opencode_plugin(bin, home.path()).expect("idempotent"));
        let text = std::fs::read_to_string(opencode_plugin_path(home.path())).expect("read");
        assert!(text.contains("/opt/conbus"));
        assert!(text.contains("session.error"));
        assert!(opencode_plugin_registered(home.path()));
    }

    #[test]
    fn cursor_native_hooks_append_and_leave_the_rest() {
        let existing = json!({
            "version": 1,
            "hooks": {
                "sessionEnd": [{ "command": "their-session-end" }],
                "afterAgentResponse": [{ "command": "their-notify" }]
            }
        });
        let (merged, changes) = merge_cursor(
            &existing,
            &CURSOR_EVENTS,
            "/opt/conbus hook --harness cursor",
        );

        assert_eq!(
            merged["hooks"]["sessionEnd"][0]["command"],
            "their-session-end"
        );
        assert_eq!(
            merged["hooks"]["sessionEnd"][1]["command"],
            "/opt/conbus hook --harness cursor"
        );
        assert_eq!(
            merged["hooks"]["afterAgentResponse"][0]["command"],
            "their-notify"
        );
        assert_eq!(
            merged["hooks"]["afterAgentResponse"][1]["command"],
            "/opt/conbus hook --harness cursor"
        );
        assert_eq!(
            changes.iter().map(Change::event).collect::<Vec<_>>(),
            [
                "sessionStart",
                "beforeSubmitPrompt",
                "stop",
                "sessionEnd",
                "afterAgentResponse",
            ]
        );
        assert!(changes.iter().all(|c| matches!(c, Change::Add(_))));
    }

    #[test]
    fn cursor_native_hooks_do_not_register_twice() {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = Path::new("/opt/conbus");
        assert!(apply_cursor_hooks(bin, home.path()).expect("write"));
        assert!(!apply_cursor_hooks(bin, home.path()).expect("idempotent"));
        assert!(cursor_hooks_registered(home.path()));
        let text = std::fs::read_to_string(cursor_hooks_path(home.path())).expect("read");
        assert_eq!(text.matches("afterAgentResponse").count(), 1);
        assert_eq!(text.matches("sessionEnd").count(), 1);
        assert_eq!(text.matches("sessionStart").count(), 1);
        assert_eq!(text.matches("beforeSubmitPrompt").count(), 1);
        assert_eq!(text.matches("\"stop\"").count(), 1);
        assert!(text.contains("/opt/conbus hook --harness cursor"));
    }

    #[test]
    fn the_registered_command_is_absolute() {
        // A vendor's PATH is not ours: Codex's app-server in particular runs
        // with a different one, which is why relative registration fails there.
        let (merged, _) = merge(&empty_document(), &CODEX_EVENTS, CMD);

        let command = merged["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .expect("command");
        assert!(command.starts_with('/'), "{command}");
    }

    #[test]
    fn a_statusline_the_person_already_has_is_wrapped_not_replaced() {
        let existing = json!({
            "statusLine": { "type": "command", "command": "their-status" }
        });
        let mut document = existing.clone();
        let change = merge_statusline(&mut document, "/opt/conbus hook --statusline");

        assert!(matches!(change, Some(Change::Add(name)) if name == "StatusLine"));
        assert_eq!(
            document["statusLine"]["command"],
            "/opt/conbus hook --statusline"
        );
        assert_eq!(existing["statusLine"]["command"], "their-status");
    }

    #[test]
    fn wrapping_the_statusline_keeps_the_persons_other_settings_on_it() {
        let mut document = json!({
            "statusLine": { "type": "command", "command": "their-status", "padding": 0 }
        });
        merge_statusline(&mut document, "/opt/conbus hook --statusline");

        assert_eq!(
            document["statusLine"],
            json!({ "type": "command", "command": "/opt/conbus hook --statusline", "padding": 0 })
        );
    }

    #[test]
    fn a_hook_command_is_ours_by_program_name_wherever_it_was_built() {
        assert!(is_conbus_hook("/Users/x/.local/bin/conbus hook"));
        assert!(is_conbus_hook(
            "/Applications/conbus/target/release/conbus hook --harness codex"
        ));
        assert!(is_conbus_hook("/opt/conbus hook --statusline"));
        assert!(!is_conbus_hook("their-own-stop"));
        assert!(!is_conbus_hook("/opt/not-conbus hook"));
        assert!(!is_conbus_hook("paseo hooks codex Stop"));
    }

    #[test]
    fn installing_statusline_twice_does_not_wrap_again() {
        let mut document = json!({
            "statusLine": { "type": "command", "command": "/opt/conbus hook --statusline" }
        });
        let change = merge_statusline(&mut document, "/opt/conbus hook --statusline");
        assert!(matches!(change, Some(Change::Keep(name)) if name == "StatusLine"));
        assert_eq!(
            document["statusLine"]["command"],
            "/opt/conbus hook --statusline"
        );
    }

    #[test]
    fn a_replaced_file_is_never_observed_half_written() {
        // The reason the binary has been renamed into place since the
        // two-hour outage, applied to the files a person already owned. A
        // truncating write is visible to a reader in the middle; a rename is
        // not. The reader below only ever misses a defect, it cannot invent
        // one, so a green run is weak evidence and a red run is proof.
        use std::sync::Arc;
        use std::sync::atomic::{AtomicBool, Ordering};

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        let old = "o".repeat(512 * 1024);
        let new = "n".repeat(512 * 1024);
        fs::write(&path, &old).expect("seed");

        let stop = Arc::new(AtomicBool::new(false));
        let watching = Arc::clone(&stop);
        let watched = path.clone();
        let (was_old, was_new) = (old.clone(), new.clone());
        let reader = std::thread::spawn(move || {
            let mut torn = 0usize;
            while !watching.load(Ordering::Relaxed) {
                if let Ok(seen) = fs::read_to_string(&watched)
                    && seen != was_old
                    && seen != was_new
                {
                    torn += 1;
                }
            }
            torn
        });

        for round in 0..8 {
            let contents = if round % 2 == 0 { &new } else { &old };
            write_atomically(&path, contents).expect("replace");
        }
        stop.store(true, Ordering::Relaxed);

        assert_eq!(
            reader.join().expect("reader"),
            0,
            "a reader saw the file part-written"
        );
    }

    #[test]
    fn a_replace_that_fails_leaves_the_original_and_nothing_beside_it() {
        // Something already sitting where the temp file goes is the cheapest
        // failure to arrange, and it exercises the path that matters: the
        // target must not be touched, and the run must not leave litter for
        // the next one to trip over.
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("settings.json");
        fs::write(&path, "{\"kept\": true}").expect("seed");
        let blocked = dir.path().join("settings.json.conbus-next");
        fs::create_dir(&blocked).expect("block the temp name");

        let outcome = write_atomically(&path, "{\"replaced\": true}");

        assert!(outcome.is_err(), "the write cannot have succeeded");
        assert_eq!(
            fs::read_to_string(&path).expect("still there"),
            "{\"kept\": true}",
            "a failed replace must not touch the target"
        );
        assert!(blocked.is_dir(), "the obstruction is not ours to delete");
    }

    #[test]
    fn a_replaced_file_keeps_the_mode_it_had() {
        // The StatusLine forward is a script. Replacing it with a fresh file
        // would silently take its execute bit away.
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("conbus-statusline-forward");
        fs::write(&path, "#!/bin/sh\nexit 0\n").expect("seed");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod");

        write_atomically(&path, "#!/bin/sh\nexit 1\n").expect("replace");

        let mode = fs::metadata(&path).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o755, "the execute bit was dropped");
    }

    #[test]
    fn placing_the_binary_writes_beside_then_renames() {
        let home = tempfile::tempdir().expect("tempdir");
        let src = home.path().join("built");
        fs::write(&src, b"conbus-bin").expect("src");
        let dest = installed_binary(home.path());
        assert!(place_binary(&src, &dest).expect("place"));
        assert_eq!(fs::read(&dest).expect("dest"), b"conbus-bin");
        assert!(!place_binary(&src, &dest).expect("same bytes"));
    }

    #[test]
    fn the_launchd_plist_names_the_installed_binary_and_never_async() {
        let body = daemon_plist(
            Path::new("/Users/x/.local/bin/conbus"),
            Path::new("/Users/x/.conbus/daemon.log"),
        );
        assert!(body.contains("/Users/x/.local/bin/conbus"));
        assert!(body.contains("<string>daemon</string>"));
        assert!(body.contains(DAEMON_LABEL));
        assert!(body.contains("KeepAlive"));
        assert!(!body.contains("async"));
    }

    #[test]
    fn applying_the_daemon_is_idempotent_without_talking_to_launchctl() {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = Path::new("/opt/conbus");
        let log = home.path().join(".conbus").join("daemon.log");
        assert!(apply_daemon(home.path(), bin, &log, false).expect("write"));
        assert!(!apply_daemon(home.path(), bin, &log, false).expect("again"));
        let written = fs::read_to_string(launch_agent_path(home.path())).expect("plist");
        assert_eq!(written, daemon_plist(bin, &log));
    }
}

#[cfg(test)]
mod registration_agrees_with_the_adapters {
    use super::*;
    use crate::hook::{Normalized, claude_code, codex};
    use conbus_core::event::Harness;

    /// The vendors' own event lists, read from the contracts extracted from
    /// their binaries.
    ///
    /// Read at compile time from the checked-in fixtures rather than restated
    /// here, so that re-extracting a contract after a vendor release makes the
    /// tests below fail until someone decides what the new event is. A list
    /// maintained by hand beside the code it checks is a list that agrees with
    /// the code by construction.
    const CLAUDE_CONTRACT: &str = include_str!("../tests/fixtures/claude_code_2.1.241_contract.md");
    const CODEX_CONTRACT: &str = include_str!("../tests/fixtures/codex_0.147.0_contract.md");

    /// Every `### EventName` heading under the contract's payload section.
    fn events_in(contract: &str) -> Vec<&str> {
        contract
            .lines()
            .filter_map(|line| line.strip_prefix("### "))
            .map(str::trim)
            .filter(|name| {
                !name.is_empty()
                    && name.chars().all(|c| c.is_ascii_alphanumeric())
                    && name.starts_with(char::is_uppercase)
            })
            .collect()
    }

    /// A payload carrying nothing but an event name, which is all the adapters
    /// need in order to decide what an event is.
    fn classify(harness: &str, event: &str) -> &'static str {
        let payload = format!(r#"{{"session_id":"s","hook_event_name":"{event}"}}"#);
        match crate::hook::normalize(&Harness::new(harness), &payload).expect("normalise") {
            Normalized::Event { .. } => "mirrored",
            Normalized::Ignored { .. } => "declined",
            Normalized::Unknown { .. } => "unknown",
        }
    }

    struct Harnessed {
        harness: &'static str,
        contract: &'static str,
        registered: &'static [&'static str],
        declined: &'static [&'static str],
    }

    fn harnesses() -> [Harnessed; 2] {
        [
            Harnessed {
                harness: Harness::CLAUDE_CODE,
                contract: CLAUDE_CONTRACT,
                registered: CLAUDE_EVENTS.as_slice(),
                declined: claude_code::DECLINED.as_slice(),
            },
            Harnessed {
                harness: Harness::CODEX,
                contract: CODEX_CONTRACT,
                registered: CODEX_EVENTS.as_slice(),
                declined: codex::DECLINED.as_slice(),
            },
        ]
    }

    /// The invariant that was missing, and the reason a mirrored event sat dead
    /// for a whole commit.
    ///
    /// An adapter arm does nothing unless the event is also registered as a
    /// hook, and a registration does nothing unless the adapter maps it. The
    /// first version of this test iterated the two lists we maintain, which
    /// cannot see an event that fell out of both — which is exactly what
    /// happened. It iterates the *vendor's* list now.
    #[test]
    fn every_vendor_event_is_either_registered_and_mirrored_or_declined_and_ignored() {
        for h in harnesses() {
            let events = events_in(h.contract);
            assert!(
                events.len() > 5,
                "{}: contract parsed as {events:?}",
                h.harness
            );

            for event in events {
                let registered = h.registered.contains(&event);
                let declined = h.declined.contains(&event);
                let classified = classify(h.harness, event);

                assert!(
                    registered ^ declined,
                    "{}: {event} is {} — every vendor event must be exactly one of registered or                      declined, or it is silently dropped on a real machine",
                    h.harness,
                    if registered { "both" } else { "neither" }
                );
                let expected = if registered { "mirrored" } else { "declined" };
                assert_eq!(
                    classified,
                    expected,
                    "{}: {event} is {} but the adapter treats it as {classified}",
                    h.harness,
                    if registered { "registered" } else { "declined" }
                );
            }
        }
    }

    #[test]
    fn nothing_is_registered_or_declined_that_the_vendor_does_not_send() {
        for h in harnesses() {
            let events = events_in(h.contract);
            for named in h.registered.iter().chain(h.declined.iter()) {
                assert!(
                    events.contains(named),
                    "{}: {named} is named by conbus but is not in the vendor's contract",
                    h.harness
                );
            }
        }
    }
}
