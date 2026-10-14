//! `conbus install --managed`: the hooks an administrator puts in Claude
//! Code's managed settings, for machines whose policy sets
//! `allowManagedHooksOnly`.
//!
//! That lock blocks the user-level hooks `install --apply` registers, so on a
//! managed fleet — the first buyer in `docs/saas-strategy.md` — conbus records
//! nothing unless its hooks sit in the organisation's own policy. This module
//! builds that block and says where it goes. It writes nothing, `--apply` or
//! not: managed settings belong to the organisation's device tooling, and a
//! program that edited them itself would be doing the very thing that policy
//! exists to control.
//!
//! The block is not restated here. It is `install`'s own merge of `install`'s
//! own Claude Code events and command into an empty document, so it cannot
//! drift from what `install --apply` writes into `~/.claude/settings.json`;
//! only the binary differs — the one the administrator ran, because one
//! managed file serves every account on every Mac.
//!
//! Every path, key and rule below was checked against the vendors' own
//! documentation on 2026-09-24, and the guidance prints the pages with them:
//! Claude Code's managed settings, settings reference and hooks pages; Codex's
//! hooks and managed configuration pages; Cursor's hooks page.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

use super::{CLAUDE_EVENTS, CODEX_EVENTS, CURSOR_EVENTS, empty_document, hook_command, merge};

/// Claude Code's managed settings directory: `/Library/Application
/// Support/ClaudeCode` on a Mac, `/etc/claude-code` on Linux.
/// `managed-settings.json` lives here, and the `managed-settings.d/` drop-ins
/// beside it.
#[cfg(target_os = "macos")]
pub const CLAUDE_MANAGED_DIR: &str = "/Library/Application Support/ClaudeCode";
#[cfg(not(target_os = "macos"))]
pub const CLAUDE_MANAGED_DIR: &str = "/etc/claude-code";

/// The managed preferences domain a configuration profile delivers Claude
/// Code's policy through: the same keys as `managed-settings.json`. A macOS
/// mechanism — Linux fleets write the directory, not a profile.
#[cfg(target_os = "macos")]
pub const CLAUDE_PROFILE_DOMAIN: &str = "com.anthropic.claudecode";

/// Codex's system requirements file on macOS and Linux. Managed hooks go
/// under its `[hooks]` table.
pub const CODEX_REQUIREMENTS: &str = "/etc/codex/requirements.toml";

/// Codex's managed preferences domain, and the key that carries a
/// base64-encoded `requirements.toml`. A macOS configuration-profile
/// mechanism.
#[cfg(target_os = "macos")]
pub const CODEX_PROFILE_DOMAIN: &str = "com.openai.codex";
#[cfg(target_os = "macos")]
pub const CODEX_PROFILE_KEY: &str = "requirements_toml_base64";

/// Cursor's enterprise hooks file: `/Library/Application
/// Support/Cursor/hooks.json` on a Mac, `/etc/cursor/hooks.json` on Linux —
/// system-wide, placed by the organisation's device tooling.
#[cfg(target_os = "macos")]
pub const CURSOR_ENTERPRISE_HOOKS: &str = "/Library/Application Support/Cursor/hooks.json";
#[cfg(not(target_os = "macos"))]
pub const CURSOR_ENTERPRISE_HOOKS: &str = "/etc/cursor/hooks.json";

const CLAUDE_DOCS: &str = "https://code.claude.com/docs/en/managed-settings";
const CODEX_DOCS: &str =
    "https://developers.openai.com/codex/hooks#managed-hooks-from-requirementstoml";
const CURSOR_DOCS: &str = "https://cursor.com/docs/hooks#configuration";

/// How wide a wrapped event list may run inside a sub-list.
const LIST_WIDTH: usize = 64;

/// The Claude Code block: the `hooks` key and nothing else.
///
/// Built with `merge`, not `plan`, because `plan` also wraps the StatusLine,
/// and that wrapper stays out of managed settings on purpose, for three
/// reasons the vendor documents:
///
/// - `statusLine` is one value, not a list. Managed settings sit above every
///   other level, so a managed one replaces the status line of every developer
///   the policy reaches — and in a `managed-settings.d` drop-in, where a later
///   file's single value replaces an earlier one, the organisation's own.
/// - Under `allowManagedHooksOnly`, Claude Code reads `statusLine` from
///   managed settings only. conbus's wrapper keeps a person's own status bar
///   by running the command it saved in their
///   `~/.claude/conbus-statusline-forward`, so a managed wrapper would run a
///   command from a user-writable file under the organisation's authority:
///   exactly what that narrowing exists to stop.
/// - What the wrapper observes, `rate_limits`, reaches a status line only for
///   claude.ai Pro and Max subscribers or behind a gateway with spend limits,
///   so not for the Team, Enterprise and API seats a managed fleet runs on.
///
/// Nor does the block carry `allowManagedHooksOnly` itself. The lock is the
/// organisation's to set, and a drop-in that set it would overrule theirs.
pub fn claude_code_block(binary: &Path) -> Value {
    merge(
        &empty_document(),
        &CLAUDE_EVENTS,
        &hook_command(binary, "claude_code"),
    )
    .0
}

/// What `install --managed` prints.
pub struct Printed {
    /// The whole of stdout: one JSON document, so it can be redirected
    /// straight into a file. Claude Code refuses to start on a Mac whose
    /// managed settings file does not parse, so nothing else reaches stdout.
    pub block: String,
    /// Stderr: where the block goes and how it merges, pointers for Codex and
    /// Cursor, and what stays per user.
    pub guidance: String,
}

/// Everything `install --managed` prints for `binary`, which is written into
/// every command. `home` is only asked whether the binary is inside it.
pub fn printed(binary: &Path, home: Option<&Path>, apply: bool) -> io::Result<Printed> {
    let block =
        serde_json::to_string_pretty(&claude_code_block(binary)).map_err(io::Error::other)?;
    let guidance = [
        warnings(binary, home, apply),
        claude_code_summary(binary),
        section("where", &claude_code_where()),
        section("merge", &claude_code_merge()),
        section("statusline", &claude_code_statusline()),
        section("docs", &[CLAUDE_DOCS.into()]),
        section("codex", &codex_pointer(binary)),
        section("cursor", &cursor_pointer(binary)),
        section("daemon", &per_user()),
    ]
    .concat();
    Ok(Printed {
        block: format!("{block}\n"),
        guidance,
    })
}

/// `conbus install --managed [--apply]`. Prints, and writes nothing.
///
/// The binary is the one running, made absolute but not resolved: a symlink
/// such as `/usr/local/bin/conbus` stays the name an upgrade keeps, where its
/// target may not. HOME is optional, because an MDM script may run without it.
pub fn run(apply: bool) -> io::Result<()> {
    let binary = std::path::absolute(std::env::current_exe()?)?;
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let printed = printed(&binary, home.as_deref(), apply)?;
    eprint!("{}", printed.guidance);
    print!("{}", printed.block);
    Ok(())
}

/// Lines under a label, the way `install` lays out its report.
fn section(label: &str, lines: &[String]) -> String {
    lines
        .iter()
        .enumerate()
        .map(|(index, line)| {
            let label = if index == 0 { label } else { "" };
            format!("{label:<12} {line}\n")
        })
        .collect()
}

/// A labelled sub-list, its continuation lines indented under its first.
fn sub(label: &str, lines: Vec<String>) -> Vec<String> {
    lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let label = if index == 0 { label } else { "" };
            format!("{label:<8} {line}")
        })
        .collect()
}

/// A list joined with commas and broken into lines no wider than `width`; a
/// line that continues ends with its comma.
fn wrapped(items: &[&str], width: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for item in items {
        // Room for this item and for the comma a later line would add.
        let fits = lines
            .last()
            .is_some_and(|line| line.len() + ", ".len() + item.len() < width);
        match lines.last_mut() {
            Some(line) if fits => {
                line.push_str(", ");
                line.push_str(item);
            }
            Some(line) => {
                line.push(',');
                lines.push((*item).to_string());
            }
            None => lines.push((*item).to_string()),
        }
    }
    lines
}

/// Whether every character of a path means itself to `sh -c`, which is how
/// the vendors run a hook command. The set `uninstall` leaves unquoted.
fn shell_safe(path: &str) -> bool {
    path.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-".contains(c))
}

fn warnings(binary: &Path, home: Option<&Path>, apply: bool) -> String {
    let shown = binary.to_string_lossy();
    let in_home = home.is_some_and(|home| home.parent().is_some() && binary.starts_with(home));
    let home_warning = [
        format!("{shown} is inside this account's home, and one"),
        "managed file serves every account on every machine. Place the binary where all".into(),
        "of them can run it, such as /usr/local/bin/conbus, and run this from there.".into(),
    ];
    let shell_warning = [
        format!("{shown} has characters a shell splits or expands,"),
        "so the hook command would not run. Use a path of letters, digits, / . _ -".into(),
    ];
    let apply_note = [
        "--apply does nothing here: conbus never writes managed settings, which".into(),
        "belong to your organisation's own device tooling.".into(),
    ];
    [
        in_home.then(|| section("warning", &home_warning)),
        (!shell_safe(&shown)).then(|| section("warning", &shell_warning)),
        apply.then(|| section("note", &apply_note)),
    ]
    .into_iter()
    .flatten()
    .collect()
}

fn claude_code_summary(binary: &Path) -> String {
    section(
        "claude_code",
        &[
            "stdout is the hooks block for Claude Code's managed settings. Nothing was".into(),
            "written. It is the registration install --apply writes to".into(),
            format!(
                "~/.claude/settings.json — {} events — with this binary in each command:",
                CLAUDE_EVENTS.len()
            ),
            hook_command(binary, "claude_code"),
        ],
    )
}

fn claude_code_where() -> Vec<String> {
    #[cfg(target_os = "macos")]
    let sources = vec![
        "Claude Code applies one managed source, the first that has a policy key:".into(),
        format!("server-managed settings, then a {CLAUDE_PROFILE_DOMAIN} profile,"),
        format!("then the files in {CLAUDE_MANAGED_DIR}, unless"),
        "managedSourcesBehavior is \"merge\". Use the one your Macs get; /status names it.".into(),
    ];
    #[cfg(not(target_os = "macos"))]
    let sources = vec![
        "Claude Code applies one managed source, the first that has a policy key:".into(),
        format!("server-managed settings, then the files in {CLAUDE_MANAGED_DIR}, unless"),
        "managedSourcesBehavior is \"merge\". Use the one your machines get;".into(),
        "/status names it.".into(),
    ];
    [
        sources,
        sub(
            "file",
            vec![
                format!("{CLAUDE_MANAGED_DIR}/managed-settings.d/conbus.json,"),
                "read after managed-settings.json in name order; hook lists add up".into(),
            ],
        ),
        sub(
            "file",
            vec![
                "or these entries added to \"hooks\" in".into(),
                format!("{CLAUDE_MANAGED_DIR}/managed-settings.json"),
            ],
        ),
        #[cfg(target_os = "macos")]
        sub(
            "profile",
            vec![
                "the same keys as plist dictionaries and arrays".into(),
                "(plutil -convert xml1 -o - FILE prints them)".into(),
            ],
        ),
    ]
    .concat()
}

fn claude_code_merge() -> Vec<String> {
    [
        "hooks from every level run side by side, so the organisation's own keep",
        "running. With allowManagedHooksOnly true, only managed hooks run and this",
        "block is the copy that records. Without it you need no block: install --apply",
        "registers the same events per user, and since the two copies name different",
        "binaries, Claude Code would run both and every event would be recorded twice.",
    ]
    .map(String::from)
    .to_vec()
}

fn claude_code_statusline() -> Vec<String> {
    [
        "left out on purpose. A managed statusLine replaces every developer's, and",
        "conbus's wrapper runs the command each person saved in their ~/.claude —",
        "what allowManagedHooksOnly keeps from running. Its rate_limits reach Pro and",
        "Max plans only.",
    ]
    .map(String::from)
    .to_vec()
}

fn codex_pointer(binary: &Path) -> Vec<String> {
    let directory = binary.parent().unwrap_or(binary).display();
    #[cfg(target_os = "macos")]
    let where_it_goes = vec![
        format!("managed hooks: [hooks] in {CODEX_REQUIREMENTS}, or the profile key"),
        format!("{CODEX_PROFILE_KEY} for {CODEX_PROFILE_DOMAIN}. Codex trusts those"),
        "by policy, where the copy install --apply writes waits until each developer".into(),
        "trusts it in /hooks. conbus writes neither.".into(),
    ];
    #[cfg(not(target_os = "macos"))]
    let where_it_goes = vec![
        format!("managed hooks: [hooks] in {CODEX_REQUIREMENTS}. Codex trusts it"),
        "by policy, where the copy install --apply writes waits until each developer".into(),
        "trusts it in /hooks. conbus writes neither.".into(),
    ];
    [
        where_it_goes,
        sub("events", wrapped(&CODEX_EVENTS, LIST_WIDTH)),
        sub(
            "command",
            vec![format!("{} (not async)", hook_command(binary, "codex"))],
        ),
        sub(
            "also",
            vec![
                format!("managed_dir = \"{directory}\", which must exist, and"),
                "allow_managed_hooks_only = true, so the per-user copy does not run too".into(),
            ],
        ),
        sub("docs", vec![CODEX_DOCS.into()]),
    ]
    .concat()
}

fn cursor_pointer(binary: &Path) -> Vec<String> {
    [
        vec![
            format!("managed hooks: {CURSOR_ENTERPRISE_HOOKS}, placed by"),
            "your MDM, or team hooks from Cursor's dashboard (Enterprise). Cursor runs".into(),
            "hooks from every source, and install --apply registers ~/.cursor/hooks.json".into(),
            "for each person who runs it, so a managed copy as well records every event".into(),
            "twice.".into(),
        ],
        sub("events", wrapped(&CURSOR_EVENTS, LIST_WIDTH)),
        sub("command", vec![hook_command(binary, "cursor")]),
        sub("docs", vec![CURSOR_DOCS.into()]),
    ]
    .concat()
}

fn per_user() -> Vec<String> {
    [
        "the recorder runs per user: each person runs `conbus install --apply` once,",
        #[cfg(target_os = "macos")]
        "which writes and loads ~/Library/LaunchAgents/sh.conbus.daemon.plist.",
        #[cfg(not(target_os = "macos"))]
        "which writes and enables ~/.config/systemd/user/sh.conbus.daemon.service.",
        "docs/fleet.md covers approving it by MDM and taking everything back out.",
    ]
    .map(String::from)
    .to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    const FLEET: &str = "/usr/local/bin/conbus";

    #[test]
    fn the_block_is_the_merge_install_writes_into_claude_settings() {
        // No drift by construction: the registration `install --apply` makes
        // for Claude Code, merged into an empty document by the same function.
        let home = tempfile::tempdir().expect("tempdir");
        let binary = Path::new(FLEET);
        let target = super::super::targets(binary, home.path())
            .into_iter()
            .find(|target| target.harness == "claude_code")
            .expect("claude code target");

        let (expected, _) = merge(&empty_document(), target.events, &target.command);

        assert_eq!(claude_code_block(binary), expected);
    }

    #[test]
    fn the_block_holds_the_hooks_and_no_policy_of_its_own() {
        let block = claude_code_block(Path::new(FLEET));

        let keys: Vec<&String> = block.as_object().expect("object").keys().collect();
        assert_eq!(keys, ["hooks"]);
        let text = block.to_string();
        for absent in ["statusLine", "allowManagedHooksOnly", "async", "matcher"] {
            assert!(!text.contains(absent), "{absent}: {text}");
        }
        let events = block["hooks"].as_object().expect("events");
        assert_eq!(events.len(), CLAUDE_EVENTS.len());
        let one = serde_json::json!([
            { "hooks": [{ "type": "command", "command": "/usr/local/bin/conbus hook" }] }
        ]);
        for groups in events.values() {
            assert_eq!(groups, &one);
        }
    }

    #[test]
    fn stdout_is_one_json_document_whatever_the_flags() {
        let quiet = printed(Path::new(FLEET), None, false).expect("print");
        let applied = printed(Path::new(FLEET), None, true).expect("print");

        let parsed: Value = serde_json::from_str(&quiet.block).expect("json");
        assert_eq!(parsed, claude_code_block(Path::new(FLEET)));
        assert_eq!(applied.block, quiet.block, "--apply must change nothing");
        assert!(
            applied.guidance.contains("--apply does nothing here"),
            "{}",
            applied.guidance
        );
        assert!(!quiet.guidance.contains("--apply does nothing"));
    }

    #[test]
    fn a_binary_every_account_can_run_draws_no_warning() {
        let out = printed(Path::new(FLEET), Some(Path::new("/Users/admin")), false).expect("print");

        assert!(!out.guidance.contains("warning"), "{}", out.guidance);
    }

    #[test]
    fn a_binary_inside_a_home_is_named_as_the_wrong_one_for_a_fleet() {
        let out = printed(
            Path::new("/Users/admin/.local/bin/conbus"),
            Some(Path::new("/Users/admin")),
            false,
        )
        .expect("print");

        assert!(
            out.guidance.starts_with("warning"),
            "the warning comes first: {}",
            out.guidance
        );
        assert!(out.guidance.contains("inside this account's home"));
        let rooted = printed(Path::new(FLEET), Some(Path::new("/")), false).expect("print");
        assert!(
            !rooted.guidance.contains("warning"),
            "a HOME of / would put every path inside it"
        );
    }

    #[test]
    fn a_path_a_shell_would_split_is_warned_about() {
        // Every vendor runs the command through a shell, and the command
        // carries the path unquoted, as `install --apply` registers it.
        let out = printed(
            Path::new("/Library/Application Support/conbus/conbus"),
            None,
            false,
        )
        .expect("print");

        assert!(
            out.guidance.contains("characters a shell splits"),
            "{}",
            out.guidance
        );
        assert!(shell_safe(FLEET));
        assert!(!shell_safe("/opt/$HOME/conbus"));
    }

    #[test]
    fn the_codex_and_cursor_pointers_carry_what_install_registers_for_them() {
        let binary = Path::new(FLEET);
        let guidance = printed(binary, None, false).expect("print").guidance;

        for event in CODEX_EVENTS.iter().chain(CURSOR_EVENTS.iter()) {
            assert!(guidance.contains(event), "{event} missing:\n{guidance}");
        }
        for harness in ["codex", "cursor"] {
            let command = hook_command(binary, harness);
            assert!(
                guidance.contains(&command),
                "{command} missing:\n{guidance}"
            );
        }
        assert!(
            guidance.contains("managed_dir = \"/usr/local/bin\""),
            "{guidance}"
        );
        assert!(guidance.contains(CODEX_REQUIREMENTS));
        assert!(guidance.contains(CURSOR_ENTERPRISE_HOOKS));
    }

    #[test]
    fn a_wrapped_list_keeps_every_item_within_its_width() {
        let lines = wrapped(&CLAUDE_EVENTS, 40);

        assert!(lines.len() > 1, "{lines:?}");
        assert!(lines.iter().all(|line| line.len() <= 40), "{lines:?}");
        assert_eq!(lines.join(" "), CLAUDE_EVENTS.join(", "));
    }
}
