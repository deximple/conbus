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

/// The recorder's service file, relative to HOME: the launchd agent on a Mac,
/// the systemd user unit on Linux.
#[cfg(target_os = "macos")]
const DAEMON_SERVICE: &str = "Library/LaunchAgents/sh.conbus.daemon.plist";
#[cfg(not(target_os = "macos"))]
const DAEMON_SERVICE: &str = ".config/systemd/user/sh.conbus.daemon.service";

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
        DAEMON_SERVICE,
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
        DAEMON_SERVICE,
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

#[test]
fn install_offers_to_replace_the_opencode_plugin_an_older_conbus_wrote() {
    // The plugin is wholly ours, and what it does changes with the build.
    // `install` used to call it registered whenever it named the events it
    // forwards — which the 0.2.3 plugin does — so a fixed plugin never
    // reached a machine that already had one.
    let machine = Machine::new();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_conbus"));
    let plugin = machine.at(".config/opencode/plugins/conbus.js");
    let older = include_str!("fixtures/opencode_plugin_0.2.3.js");
    std::fs::create_dir_all(plugin.parent().expect("plugins")).expect("plugins");
    std::fs::write(&plugin, older).expect("the older plugin");

    let report = conbus(&built, &machine.home, &["install"]);

    let line = report
        .lines()
        .find(|line| line.starts_with("opencode"))
        .unwrap_or_else(|| panic!("no opencode line:\n{report}"));
    assert!(line.contains("would replace"), "{report}");
    assert_eq!(
        std::fs::read_to_string(&plugin).expect("read"),
        older,
        "a report must not write"
    );
}

/// Like `conbus`, but the exit status is the answer — for the refusal paths.
fn conbus_status(binary: &Path, home: &Path, args: &[&str]) -> (bool, String, String) {
    let out = Command::new(binary)
        .args(args)
        .env("HOME", home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env_remove("CONBUS_HOME")
        .env_remove("XDG_STATE_HOME")
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("CODEX_HOME")
        .output()
        .expect("run conbus");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn install_profile_registers_that_profile_and_nothing_else() {
    // `install --apply --profile work` is the same Claude Code registration —
    // hooks and the StatusLine wrap — aimed at $HOME/.claude-work. The default
    // profile and every other vendor stay untouched.
    let machine = Machine::new();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_conbus"));
    let theirs = json!({
        "model": "opus",
        "statusLine": { "type": "command", "command": "their-status-bar" }
    });
    write(&machine.at(".claude-work/settings.json"), &theirs);
    let default = json!({ "model": "sonnet" });
    write(&machine.at(".claude/settings.json"), &default);
    write(&machine.at(".codex/hooks.json"), &json!({ "hooks": {} }));

    let out = conbus(
        &built,
        &machine.home,
        &["install", "--apply", "--profile", "work"],
    );
    assert!(out.contains("written"), "{out}");

    let settings = read(&machine.at(".claude-work/settings.json"));
    let hooks = settings["hooks"].as_object().expect("hooks").len();
    assert!(hooks >= 10, "the full Claude registration: {settings}");
    assert!(
        settings["statusLine"]["command"]
            .as_str()
            .is_some_and(|c| c.ends_with("conbus hook --statusline")),
        "{settings}"
    );
    assert_eq!(settings["model"], "opus", "their keys kept: {settings}");
    assert_eq!(
        std::fs::read_to_string(machine.at(".claude-work/conbus-statusline-forward"))
            .expect("forward"),
        "their-status-bar"
    );
    assert_eq!(
        read(&machine.at(".claude/settings.json")),
        default,
        "the default profile's settings are the default profile's"
    );
    assert_eq!(
        read(&machine.at(".codex/hooks.json")),
        json!({ "hooks": {} }),
        "codex is the machine's, not the profile's"
    );
    assert!(!machine.at(".claude/conbus-statusline-forward").exists());
}

#[test]
fn install_profile_refuses_a_directory_that_is_not_there() {
    // A profile exists only once the vendor's own login created its
    // directory; conbus never creates it for them.
    let machine = Machine::new();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_conbus"));

    let (ok, _out, err) = conbus_status(
        &built,
        &machine.home,
        &["install", "--apply", "--profile", "work"],
    );

    assert!(!ok, "a missing profile directory must refuse");
    let want = format!(
        "CLAUDE_CONFIG_DIR={} claude",
        machine.at(".claude-work").display()
    );
    assert!(err.contains(&want), "{err}");
    assert!(err.contains("/login"), "{err}");
    assert!(!machine.at(".claude-work").exists(), "never created");

    let (ok, _out, err) = conbus_status(&built, &machine.home, &["install", "--profile", "Work"]);
    assert!(!ok);
    assert!(err.contains("[a-z0-9-]"), "{err}");
}

#[test]
fn uninstall_profile_takes_back_only_the_profile() {
    let machine = Machine::new();
    let built = PathBuf::from(env!("CARGO_BIN_EXE_conbus"));
    let theirs = json!({ "model": "opus" });
    write(&machine.at(".claude-work/settings.json"), &theirs);
    let default = json!({ "model": "sonnet" });
    write(&machine.at(".claude/settings.json"), &default);
    conbus(
        &built,
        &machine.home,
        &["install", "--apply", "--profile", "work"],
    );
    conbus(&built, &machine.home, &["install", "--apply"]);
    let installed = machine.at(".local/bin/conbus");

    let report = conbus(
        &installed,
        &machine.home,
        &["uninstall", "--profile", "work"],
    );
    assert!(report.contains("would remove"), "{report}");
    let (ok, _, _) = conbus_status(
        &installed,
        &machine.home,
        &["uninstall", "--apply", "--profile", "work"],
    );
    assert!(ok);

    assert_eq!(
        read(&machine.at(".claude-work/settings.json")),
        theirs,
        "the profile's own settings come back exactly"
    );
    assert!(
        !machine
            .at(".claude-work/conbus-statusline-forward")
            .exists()
    );
    assert!(
        read(&machine.at(".claude/settings.json"))["hooks"]
            .as_object()
            .is_some_and(|h| !h.is_empty()),
        "the default profile keeps its registration"
    );
}

#[test]
fn a_home_with_a_space_in_it_registers_hooks_that_run() {
    // Installed the way a person would, and the registered command run the way a
    // vendor runs it: through a shell, with the payload on stdin.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("first last");
    std::fs::create_dir_all(home.join(".claude")).expect("mkdir");
    write(&home.join(".claude/settings.json"), &json!({}));
    let binary = PathBuf::from(env!("CARGO_BIN_EXE_conbus"));

    conbus(&binary, &home, &["install", "--apply"]);

    let settings = read(&home.join(".claude/settings.json"));
    let command = settings["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"]
        .as_str()
        .expect("a registered command")
        .to_string();
    let mut hook = Command::new("/bin/sh")
        .args(["-c", &command])
        .env("HOME", &home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env_remove("CONBUS_HOME")
        .env_remove("XDG_STATE_HOME")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("sh");
    std::io::Write::write_all(
        hook.stdin.as_mut().expect("stdin"),
        br#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"x","prompt_id":"T"}"#,
    )
    .expect("payload");
    let out = hook.wait_with_output().expect("wait");

    assert!(out.status.success(), "{command}: {out:?}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("\"continue\":true"),
        "the hook did not run: {command}: {out:?}"
    );
}
