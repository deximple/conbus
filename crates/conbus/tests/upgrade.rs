//! An upgrade from the 0.2.x layout: `install --apply` over a HOME a 0.2.x
//! conbus already registered in must keep what the person had — their other
//! hooks, their daemon's environment — and stay a no-op the second time.

use std::path::Path;
use std::process::Command;

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

fn conbus(home: &Path, conbus_home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(bin())
        .args(args)
        .env("HOME", home)
        .env("CONBUS_HOME", conbus_home)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .output()
        .expect("run conbus")
}

fn sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_default();
    // A stable fingerprint, not a real hash — the test only compares equality.
    format!(
        "{}:{}",
        bytes.len(),
        bytes
            .iter()
            .fold(0u64, |a, b| a.wrapping_mul(31).wrapping_add(*b as u64))
    )
}

/// A plist as 0.2.x wrote it, with the operator's own environment on board.
const PLIST: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
  <key>Label</key><string>sh.conbus.daemon</string>
  <key>ProgramArguments</key>
  <array>
    <string>/usr/local/bin/conbus</string>
    <string>daemon</string>
  </array>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>EnvironmentVariables</key>
  <dict>
    <key>CONBUS_RETENTION</key><string>conversation_only</string>
  </dict>
</dict>
</plist>
"#;

#[test]
fn install_over_a_0_2_x_layout_keeps_the_persons_things() {
    let home = tempfile::tempdir().expect("tempdir");
    let conbus_home = tempfile::tempdir().expect("conbus home");
    let home = home.path();
    let conbus_home = conbus_home.path();

    // The 0.2.x layout: a Claude registration beside somebody else's hook, a
    // Codex one, the daemon's plist with its environment, the 1.x OpenCode
    // plugin file, and a log that already has records.
    let claude = home.join(".claude");
    std::fs::create_dir_all(&claude).expect("claude dir");
    std::fs::write(
        claude.join("settings.json"),
        serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/conbus hook" }] },
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/other-tool --watch" }] }
                ],
                "Stop": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/conbus hook" }] }
                ]
            }
        })
        .to_string(),
    )
    .expect("claude settings");
    let codex = home.join(".codex");
    std::fs::create_dir_all(&codex).expect("codex dir");
    std::fs::write(
        codex.join("hooks.json"),
        serde_json::json!({
            "hooks": {
                "SessionStart": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/conbus hook --harness codex" }] },
                    { "hooks": [{ "type": "command", "command": "/opt/other/thing" }] }
                ]
            }
        })
        .to_string(),
    )
    .expect("codex hooks");
    let agents = home.join("Library").join("LaunchAgents");
    std::fs::create_dir_all(&agents).expect("launchagents");
    std::fs::write(agents.join("sh.conbus.daemon.plist"), PLIST).expect("plist");
    let plugin_dir = home.join(".config").join("opencode").join("plugin");
    std::fs::create_dir_all(&plugin_dir).expect("plugin dir");
    std::fs::write(
        plugin_dir.join("conbus.js"),
        "// the 1.x plugin — a shape 2.x does not load\n",
    )
    .expect("v1 plugin");
    let log_dir = conbus_home.join("log");
    std::fs::create_dir_all(&log_dir).expect("log dir");
    std::fs::write(
        log_dir.join("segment-00000000.jsonl"),
        "{\"v\":3,\"ts\":\"2026-11-01T00:00:00.000Z\",\"seq\":0}\n",
    )
    .expect("segment");

    // First install --apply: registers, and must not eat the person's files.
    let first = conbus(home, conbus_home, &["install", "--apply"]);
    assert_eq!(
        first.status.code(),
        Some(0),
        "install failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let settings = std::fs::read_to_string(claude.join("settings.json")).expect("settings");
    assert!(
        settings.contains("other-tool"),
        "someone else's hook survived: {settings}"
    );
    assert!(
        settings.contains(".local/bin/conbus"),
        "the 0.2.x registration was repointed at the installed binary: {settings}"
    );
    let plist = std::fs::read_to_string(agents.join("sh.conbus.daemon.plist")).expect("plist");
    assert!(
        plist.contains("CONBUS_RETENTION") && plist.contains("conversation_only"),
        "the daemon's environment survived the upgrade: {plist}"
    );

    // Second install --apply: everything it owns is already there.
    let fingerprints: Vec<(String, String)> = [
        claude.join("settings.json"),
        codex.join("hooks.json"),
        agents.join("sh.conbus.daemon.plist"),
    ]
    .iter()
    .map(|p| (p.display().to_string(), sha256(p)))
    .collect();
    let second = conbus(home, conbus_home, &["install", "--apply"]);
    assert_eq!(second.status.code(), Some(0));
    for (name, before) in &fingerprints {
        assert_eq!(
            &sha256(Path::new(name)),
            before,
            "a second install --apply rewrote {name}"
        );
    }

    // uninstall --apply: conbus's entries go, the person's files stay, and
    // the log is never touched.
    let un = conbus(home, conbus_home, &["uninstall", "--apply"]);
    assert_eq!(
        un.status.code(),
        Some(0),
        "uninstall failed: {}",
        String::from_utf8_lossy(&un.stderr)
    );
    let settings = std::fs::read_to_string(claude.join("settings.json")).expect("settings");
    assert!(
        settings.contains("other-tool"),
        "the person's hook survived uninstall: {settings}"
    );
    assert!(
        !settings.contains("conbus hook"),
        "conbus's hooks were removed: {settings}"
    );
    assert!(
        log_dir.join("segment-00000000.jsonl").exists(),
        "uninstall must never touch the log"
    );
}

/// The Linux half of the same story: the unit an older conbus wrote keeps the
/// environment the operator gave it across `install --apply`, and uninstall
/// leaves the person's hooks and log alone. Runs on the Linux CI legs; macOS
/// has the plist version above.
#[cfg(target_os = "linux")]
#[test]
fn install_over_a_0_2_x_layout_keeps_the_unit_environment() {
    let home = tempfile::tempdir().expect("tempdir");
    let conbus_home = tempfile::tempdir().expect("conbus home");
    let home = home.path();
    let conbus_home = conbus_home.path();

    let claude = home.join(".claude");
    std::fs::create_dir_all(&claude).expect("claude dir");
    std::fs::write(
        claude.join("settings.json"),
        serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/conbus hook" }] },
                    { "hooks": [{ "type": "command", "command": "/usr/local/bin/other-tool --watch" }] }
                ]
            }
        })
        .to_string(),
    )
    .expect("claude settings");
    // A unit as 0.2.x wrote it, plus the operator's own Environment lines —
    // including one in systemd's multi-pair form the parser must also read.
    let units = home
        .join(".config")
        .join("systemd")
        .join("user");
    std::fs::create_dir_all(&units).expect("unit dir");
    let unit = units.join("sh.conbus.daemon.service");
    std::fs::write(
        &unit,
        "[Unit]\nDescription=conbus recorder\n\n[Service]\nExecStart=\"/usr/local/bin/conbus\" daemon\nRestart=on-failure\nRestartSec=10\nEnvironment=\"CONBUS_RETENTION=conversation_only\"\nEnvironment=\"EXTRA_ONE=keep\" \"EXTRA_TWO=also keep\"\n\n[Install]\nWantedBy=default.target\n",
    )
    .expect("0.2.x unit");
    let log_dir = conbus_home.join("log");
    std::fs::create_dir_all(&log_dir).expect("log dir");
    std::fs::write(
        log_dir.join("segment-00000000.jsonl"),
        "{\"v\":3,\"ts\":\"2026-11-01T00:00:00.000Z\",\"seq\":0}\n",
    )
    .expect("segment");

    let first = conbus(home, conbus_home, &["install", "--apply"]);
    assert_eq!(
        first.status.code(),
        Some(0),
        "install failed: {}",
        String::from_utf8_lossy(&first.stderr)
    );
    let text = std::fs::read_to_string(&unit).expect("unit");
    for kept in ["CONBUS_RETENTION=conversation_only", "EXTRA_ONE=keep"] {
        assert!(
            text.contains(kept),
            "the unit kept the operator's environment: {text}"
        );
    }
    assert!(
        text.contains(".local/bin/conbus"),
        "ExecStart was repointed at the installed binary: {text}"
    );
    assert!(
        std::fs::read_to_string(claude.join("settings.json"))
            .expect("settings")
            .contains("other-tool"),
        "someone else's hook survived"
    );

    let fingerprints: Vec<(String, String)> =
        [unit.clone(), claude.join("settings.json")]
            .iter()
            .map(|p| (p.display().to_string(), sha256(p)))
            .collect();
    let second = conbus(home, conbus_home, &["install", "--apply"]);
    assert_eq!(second.status.code(), Some(0));
    for (name, before) in &fingerprints {
        assert_eq!(
            &sha256(Path::new(name)),
            before,
            "a second install --apply rewrote {name}"
        );
    }

    let un = conbus(home, conbus_home, &["uninstall", "--apply"]);
    assert_eq!(
        un.status.code(),
        Some(0),
        "uninstall failed: {}",
        String::from_utf8_lossy(&un.stderr)
    );
    assert!(
        !unit.exists(),
        "uninstall removed conbus's own unit"
    );
    assert!(
        std::fs::read_to_string(claude.join("settings.json"))
            .expect("settings")
            .contains("other-tool"),
        "the person's hook survived uninstall"
    );
    assert!(
        log_dir.join("segment-00000000.jsonl").exists(),
        "uninstall must never touch the log"
    );
}
