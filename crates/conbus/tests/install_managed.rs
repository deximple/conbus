//! `conbus install --managed`, run the way an administrator would.
//!
//! An organisation that sets Claude Code's `allowManagedHooksOnly` blocks the
//! user-level hooks `install --apply` registers, so conbus records nothing
//! there unless its hooks are in the organisation's own managed settings. This
//! command prints that block. Four promises are pinned here: the block is JSON
//! a managed settings file can hold; it is exactly the registration
//! `install --apply` writes, event for event and command for command, so the
//! two cannot drift; it names no path but the binary's, because one file
//! serves every account on every Mac; and printing it writes nothing at all —
//! not the person's vendor files, not the launchd agent, not conbus's own data.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::SystemTime;

use serde_json::{Value, json};

fn built() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_conbus"))
}

fn run(binary: &Path, home: &Path, args: &[&str]) -> Output {
    let out = Command::new(binary)
        .args(args)
        .env("HOME", home)
        // Never the runner's or the developer's launchd.
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env_remove("CONBUS_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CODEX_HOME")
        .output()
        .expect("run conbus");
    assert!(
        out.status.success(),
        "conbus {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    out
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// The block on stdout, which has to be the whole of stdout: an administrator
/// redirects it straight into a managed settings file, and Claude Code refuses
/// to start on every Mac whose managed file does not parse.
fn managed_block(out: &Output) -> Value {
    let text = stdout(out);
    let value: Value = serde_json::from_str(&text).unwrap_or_else(|err| {
        panic!(
            "stdout is not one JSON document ({err}):\n{text}\n--- stderr ---\n{}",
            stderr(out)
        )
    });
    assert!(value.is_object(), "a settings file is an object: {value}");
    value
}

fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, serde_json::to_string_pretty(value).expect("json")).expect("write");
}

struct Machine {
    _dir: tempfile::TempDir,
    root: PathBuf,
    home: PathBuf,
}

impl Machine {
    /// A home with Claude Code on it and nothing of the person's configured.
    fn with_claude_code() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".claude")).expect("claude");
        Machine {
            _dir: dir,
            root,
            home,
        }
    }

    /// Every vendor conbus knows, each holding a file of the person's own:
    /// the worst case for a command that promises to write nothing.
    fn with_every_vendor() -> Self {
        let machine = Self::with_claude_code();
        let theirs = json!({ "hooks": { "Stop": [
            { "hooks": [{ "type": "command", "command": "their-own-stop" }] }
        ] } });
        write(&machine.at(".claude/settings.json"), &theirs);
        write(&machine.at(".codex/hooks.json"), &theirs);
        write(
            &machine.at(".cursor/hooks.json"),
            &json!({ "version": 1, "hooks": { "stop": [{ "command": "their-stop" }] } }),
        );
        std::fs::create_dir_all(machine.at(".grok")).expect("grok");
        std::fs::create_dir_all(machine.at(".config/opencode")).expect("opencode");
        std::fs::create_dir_all(machine.at("Library/LaunchAgents")).expect("agents");
        machine
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }
}

/// Every path under `root`, with its bytes and modification time — a rewrite
/// with the same bytes is still a write.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Option<(Vec<u8>, SystemTime)>> {
    let mut seen = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).expect("read dir") {
            let path = entry.expect("entry").path();
            let meta = std::fs::symlink_metadata(&path).expect("stat");
            if meta.is_dir() {
                seen.insert(path.clone(), None);
                pending.push(path);
            } else {
                let modified = meta.modified().expect("mtime");
                seen.insert(
                    path.clone(),
                    Some((std::fs::read(&path).expect("read"), modified)),
                );
            }
        }
    }
    seen
}

fn strings(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::String(text) => found.push(text.clone()),
        Value::Array(items) => items.iter().for_each(|item| strings(item, found)),
        Value::Object(fields) => fields.iter().for_each(|(key, child)| {
            found.push(key.clone());
            strings(child, found);
        }),
        _ => {}
    }
}

#[test]
fn the_managed_block_is_json_a_managed_settings_file_can_hold() {
    let machine = Machine::with_claude_code();

    let out = run(&built(), &machine.home, &["install", "--managed"]);

    let block = managed_block(&out);
    let keys: Vec<&String> = block.as_object().expect("object").keys().collect();
    assert_eq!(
        keys,
        ["hooks"],
        "only the hooks: a lock such as allowManagedHooksOnly is the organisation's \
         to set, and in a drop-in file a later value replaces theirs"
    );
}

#[test]
fn the_managed_block_is_the_registration_install_writes_with_the_absolute_command() {
    // What `install --apply` registers for Claude Code, from the file it
    // writes rather than from a list restated here.
    let installed = Machine::with_claude_code();
    run(&built(), &installed.home, &["install", "--apply"]);
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(installed.at(".claude/settings.json")).expect("read"),
    )
    .expect("parse");
    let registered = &settings["hooks"];
    assert!(
        registered
            .as_object()
            .is_some_and(|events| events.len() > 5),
        "install registered too little to compare against: {settings}"
    );

    let machine = Machine::with_claude_code();
    let block = managed_block(&run(&built(), &machine.home, &["install", "--managed"]));

    let binary = built();
    assert!(binary.is_absolute(), "{}", binary.display());
    // The same registration, with the binary the administrator ran in place of
    // the per-user copy `install --apply` points at.
    let per_user = installed.at(".local/bin/conbus").display().to_string();
    let rebased = serde_json::to_string(registered)
        .expect("serialise")
        .replace(&per_user, &binary.display().to_string());
    assert_eq!(
        block["hooks"],
        serde_json::from_str::<Value>(&rebased).expect("parse"),
        "the managed block must register every event install does, with the same command"
    );
    for (event, groups) in block["hooks"].as_object().expect("events") {
        let command = groups[0]["hooks"][0]["command"].as_str().expect("command");
        assert_eq!(
            command,
            format!("{} hook", binary.display()),
            "{event} runs something other than this binary's hook"
        );
    }
}

#[test]
fn the_managed_block_names_no_path_but_the_binary() {
    let machine = Machine::with_claude_code();

    let out = run(&built(), &machine.home, &["install", "--managed"]);

    let block = managed_block(&out);
    let mut found = Vec::new();
    strings(&block, &mut found);
    let binary = built().display().to_string();
    for text in found.iter().filter(|text| text.contains('/')) {
        assert!(
            text.starts_with(&binary),
            "one managed file serves every account on every Mac, so the only path it \
             can name is the binary's: {text}"
        );
    }
    let home = machine.home.display().to_string();
    for (stream, text) in [("stdout", stdout(&out)), ("stderr", stderr(&out))] {
        assert!(
            !text.contains(&home),
            "{stream} names this account's home:\n{text}"
        );
    }
    let text = stdout(&out);
    assert!(
        !text.contains("statusLine") && !text.contains("statusline-forward"),
        "the StatusLine wrapper runs a command from the person's own ~/.claude, which \
         allowManagedHooksOnly exists to stop, so it stays out:\n{text}"
    );
}

#[test]
fn printing_the_managed_block_writes_nothing_anywhere() {
    let machine = Machine::with_every_vendor();
    let before = snapshot(&machine.root);

    run(&built(), &machine.home, &["install", "--managed"]);
    // The dangerous spelling, too: --apply must not turn a report into a write.
    let both = run(
        &built(),
        &machine.home,
        &["install", "--managed", "--apply"],
    );

    assert_eq!(
        snapshot(&machine.root),
        before,
        "install --managed changed a file"
    );
    for absent in [
        ".local/bin/conbus",
        ".conbus",
        "Library/LaunchAgents/sh.conbus.daemon.plist",
        ".grok/hooks/conbus.json",
        ".config/opencode/plugins/conbus.js",
        ".claude/conbus-statusline-forward",
    ] {
        assert!(
            !machine.at(absent).exists(),
            "install --managed wrote {absent}"
        );
    }
    managed_block(&both);
}

#[test]
fn the_block_needs_no_home_directory() {
    // An MDM script may run with no HOME, and the block names none anyway.
    let dir = tempfile::tempdir().expect("tempdir");
    let out = Command::new(built())
        .args(["install", "--managed"])
        .current_dir(dir.path())
        .env_remove("HOME")
        .env_remove("CONBUS_HOME")
        .env_remove("XDG_STATE_HOME")
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .output()
        .expect("run conbus");

    assert!(out.status.success(), "{}", stderr(&out));
    managed_block(&out);
    assert_eq!(
        std::fs::read_dir(dir.path()).expect("read").count(),
        0,
        "nothing was written where it ran either"
    );
}

#[test]
fn the_guidance_says_where_the_block_goes_and_how_it_merges() {
    let machine = Machine::with_claude_code();

    let out = run(&built(), &machine.home, &["install", "--managed"]);

    let text = stderr(&out);
    // Claude Code's managed settings, as its own docs give them per OS; Codex
    // and Cursor have admin-managed hooks of their own.
    #[cfg(target_os = "macos")]
    let needles = [
        "/Library/Application Support/ClaudeCode/managed-settings.json",
        "managed-settings.d",
        "com.anthropic.claudecode",
        "allowManagedHooksOnly",
        "requirements.toml",
        "com.openai.codex",
        "/Library/Application Support/Cursor/hooks.json",
        // The recorder is still per user.
        "install --apply",
    ];
    #[cfg(not(target_os = "macos"))]
    let needles = [
        "/etc/claude-code/managed-settings.json",
        "managed-settings.d",
        "allowManagedHooksOnly",
        "requirements.toml",
        "/etc/cursor/hooks.json",
        "install --apply",
    ];
    for needle in needles {
        assert!(
            text.contains(needle),
            "the guidance does not mention {needle}:\n{text}"
        );
    }
}
