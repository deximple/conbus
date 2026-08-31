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
pub const CLAUDE_EVENTS: [&str; 11] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
    "PostToolUseFailure",
    "PostToolBatch",
    "PermissionDenied",
    "Stop",
    "StopFailure",
    "SubagentStop",
    "SessionEnd",
];

/// The Codex events the recorder mirrors. Its hook surface is smaller: no
/// failure, batch, or denial events exist there at all.
pub const CODEX_EVENTS: [&str; 6] = [
    "SessionStart",
    "UserPromptSubmit",
    "PreToolUse",
    "PostToolUse",
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
    let (document, changes) = merge(&existing, target.events, &target.command);
    Ok(Plan {
        target,
        document,
        changes,
        unreadable,
    })
}

/// Write the plan, keeping a copy of whatever was there before.
pub fn apply(plan: &Plan) -> io::Result<Option<PathBuf>> {
    let settings = &plan.target.settings;
    if let Some(parent) = settings.parent() {
        fs::create_dir_all(parent)?;
    }
    let backup = if settings.exists() {
        let backup = settings.with_extension("json.conbus-backup");
        fs::copy(settings, &backup)?;
        Some(backup)
    } else {
        None
    };
    let text = serde_json::to_string_pretty(&plan.document).map_err(io::Error::other)?;
    fs::write(settings, format!("{text}\n"))?;
    Ok(backup)
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
    fn the_registered_command_is_absolute() {
        // A vendor's PATH is not ours: Codex's app-server in particular runs
        // with a different one, which is why relative registration fails there.
        let (merged, _) = merge(&empty_document(), &CODEX_EVENTS, CMD);

        let command = merged["hooks"]["Stop"][0]["hooks"][0]["command"]
            .as_str()
            .expect("command");
        assert!(command.starts_with('/'), "{command}");
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
