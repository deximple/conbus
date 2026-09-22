//! A stranger's install and uninstall, end to end, in a throwaway HOME.
//!
//! The promise both commands make is about files that belong to the person:
//! everything conbus adds comes back out, and everything that was theirs is
//! exactly as it was — including their own hooks, their StatusLine and the key
//! order of their settings file. This runs the real binary the way a person
//! would: install from the build, then uninstall with the copy it installed.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

fn conbus(binary: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(binary)
        .args(args)
        .env("HOME", home)
        // Never the runner's or the developer's launchd.
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env_remove("CONBUS_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("CODEX_HOME")
        .output()
        .expect("run conbus");
    assert!(
        out.status.success(),
        "conbus {args:?} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn write(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, serde_json::to_string_pretty(value).expect("json")).expect("write");
}

fn read(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).expect("read")).expect("parse")
}

fn keys(value: &Value) -> Vec<String> {
    value.as_object().expect("object").keys().cloned().collect()
}

struct Machine {
    _dir: tempfile::TempDir,
    home: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).expect("home");
        Machine { _dir: dir, home }
    }

    fn at(&self, relative: &str) -> PathBuf {
        self.home.join(relative)
    }
}

#[test]
fn uninstall_takes_back_everything_install_wrote_and_nothing_else() {
    let machine = Machine::new();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_conbus"));

    // What the person already had. Keys deliberately not in alphabetical
    // order: a rewrite that sorts them is a rewrite that did not leave their
    // file alone.
    let claude = json!({
        "permissions": { "allow": ["Bash(ls:*)"] },
        "model": "opus",
        "hooks": { "Stop": [
            { "matcher": "*", "hooks": [{ "type": "command", "command": "their-own-stop" }] }
        ] },
        "statusLine": { "type": "command", "command": "their-status-bar", "padding": 0 }
    });
    let codex = json!({ "hooks": { "Stop": [
        { "hooks": [{ "type": "command", "command": "their-codex-stop" }] }
    ] } });
    let cursor = json!({ "version": 1, "hooks": {
        "sessionEnd": [{ "command": "their-session-end" }]
    } });
    write(&machine.at(".claude/settings.json"), &claude);
    write(&machine.at(".codex/hooks.json"), &codex);
    write(&machine.at(".cursor/hooks.json"), &cursor);
    // grok and opencode are installed too, with nothing of theirs configured.
    std::fs::create_dir_all(machine.at(".grok")).expect("grok");
    std::fs::create_dir_all(machine.at(".config/opencode")).expect("opencode");
    std::fs::create_dir_all(machine.at(".conbus/log")).expect("log");
    std::fs::write(machine.at(".conbus/log/segment-00000000.jsonl"), "{}\n").expect("seed");

    let out = conbus(&built, &machine.home, &["install", "--apply"]);
    assert!(out.contains("written"), "{out}");

    let installed = machine.at(".local/bin/conbus");
    let settings = read(&machine.at(".claude/settings.json"));
    assert_eq!(
        keys(&settings)[..2],
        ["permissions", "model"],
        "install reordered the person's settings: {settings}"
    );
    assert!(
        settings["statusLine"]["command"]
            .as_str()
            .is_some_and(|c| c.ends_with("conbus hook --statusline")),
        "{settings}"
    );
    assert_eq!(settings["statusLine"]["padding"], 0, "{settings}");
    for owned in [
        ".local/bin/conbus",
        ".grok/hooks/conbus.json",
        ".config/opencode/plugins/conbus.js",
        "Library/LaunchAgents/sh.conbus.daemon.plist",
        ".claude/conbus-statusline-forward",
    ] {
        assert!(machine.at(owned).exists(), "install did not write {owned}");
    }

    // Reporting changes nothing.
    let report = conbus(&installed, &machine.home, &["uninstall"]);
    assert!(report.contains("would remove"), "{report}");
    assert!(installed.exists());
    assert_ne!(read(&machine.at(".claude/settings.json")), claude);

    let out = conbus(&installed, &machine.home, &["uninstall", "--apply"]);

    let restored = read(&machine.at(".claude/settings.json"));
    assert_eq!(
        restored, claude,
        "the person's Claude settings did not come back"
    );
    assert_eq!(
        keys(&restored),
        keys(&claude),
        "the settings came back in another order"
    );
    assert_eq!(read(&machine.at(".codex/hooks.json")), codex);
    assert_eq!(read(&machine.at(".cursor/hooks.json")), cursor);
    for owned in [
        ".local/bin/conbus",
        ".grok/hooks/conbus.json",
        ".config/opencode/plugins/conbus.js",
        "Library/LaunchAgents/sh.conbus.daemon.plist",
        ".claude/conbus-statusline-forward",
    ] {
        assert!(!machine.at(owned).exists(), "uninstall left {owned}");
    }
    assert!(
        machine.at(".conbus/log/segment-00000000.jsonl").exists(),
        "uninstall must never touch the log"
    );
    assert!(out.contains("kept"), "{out}");

    // A second run finds nothing to do and says so.
    let again = conbus(&built, &machine.home, &["uninstall", "--apply"]);
    assert!(
        again.contains("Nothing of conbus is registered here"),
        "{again}"
    );
}

#[test]
fn uninstall_on_a_machine_conbus_never_touched_changes_nothing() {
    let machine = Machine::new();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_conbus"));
    let theirs = json!({ "model": "opus", "hooks": { "Stop": [
        { "hooks": [{ "type": "command", "command": "their-own-stop" }] }
    ] } });
    write(&machine.at(".claude/settings.json"), &theirs);
    let before = std::fs::read(machine.at(".claude/settings.json")).expect("read");

    let out = conbus(&built, &machine.home, &["uninstall", "--apply"]);

    assert!(
        out.contains("Nothing of conbus is registered here"),
        "{out}"
    );
    assert_eq!(
        std::fs::read(machine.at(".claude/settings.json")).expect("read"),
        before,
        "a file with nothing of ours in it must not even be rewritten"
    );
}

#[test]
fn install_registers_only_the_vendors_this_machine_has() {
    // Someone with only Claude Code must not get configuration directories
    // for four tools they never installed.
    let machine = Machine::new();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_conbus"));
    std::fs::create_dir_all(machine.at(".claude")).expect("claude");

    let out = conbus(&built, &machine.home, &["install", "--apply"]);

    assert!(
        machine.at(".claude/settings.json").exists(),
        "Claude Code is here and must be registered:\n{out}"
    );
    for absent in [".codex", ".grok", ".cursor", ".config/opencode"] {
        assert!(
            !machine.at(absent).exists(),
            "install created {absent} for a vendor that is not installed:\n{out}"
        );
    }
    assert!(out.contains("codex        not found"), "{out}");
}
