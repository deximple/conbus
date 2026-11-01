//! The OpenCode plugin `install` writes.
//!
//! OpenCode has no hook settings to merge into. It loads every file in its
//! plugins directory at startup, so the recorder is a small plugin of its own,
//! wholly conbus's, that forwards official plugin-bus events to
//! `conbus hook --harness opencode`.

use std::io;
use std::path::{Path, PathBuf};

use super::write_atomically;

/// Official OpenCode plugin path. Local plugins in this directory load at startup.
pub fn opencode_plugin_path(home: &Path) -> PathBuf {
    home.join(".config")
        .join("opencode")
        .join("plugins")
        .join("conbus.js")
}

/// The plugin, with one slot for the binary it runs.
const PLUGIN: &str = include_str!("opencode_plugin.js");
/// The slot, spelled as the JavaScript string literal that fills it.
const BINARY_SLOT: &str = r#""@CONBUS_BIN@""#;

/// Plugin that forwards official bus events to `conbus hook --harness opencode`.
pub fn opencode_plugin_source(binary: &Path) -> String {
    let spelled = serde_json::Value::String(binary.display().to_string()).to_string();
    PLUGIN.replace(BINARY_SLOT, &spelled)
}

/// Whether a conbus plugin is in place, whichever build wrote it.
pub fn opencode_plugin_registered(home: &Path) -> bool {
    std::fs::read_to_string(opencode_plugin_path(home))
        .is_ok_and(|text| text.contains("opencode_plugin") && text.contains("session.error"))
}

/// Whether the plugin in place is the one this build writes.
///
/// Registered is not enough: the file is wholly conbus's and what it does
/// changes with the build. Asking only whether a plugin forwards the right
/// events left the 0.2.3 plugin — one `turn_started` per agent step — in
/// place through every later `install --apply`.
pub fn opencode_plugin_current(binary: &Path, home: &Path) -> bool {
    std::fs::read_to_string(opencode_plugin_path(home))
        .is_ok_and(|text| text == opencode_plugin_source(binary))
}

/// Write the OpenCode plugin. Idempotent when the command path is unchanged.
pub fn apply_opencode_plugin(binary: &Path, home: &Path) -> io::Result<bool> {
    if opencode_plugin_current(binary, home) {
        return Ok(false);
    }
    write_atomically(&opencode_plugin_path(home), &opencode_plugin_source(binary))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::fs;
    use std::process::{Child, Command, Output, Stdio};
    use std::time::{Duration, Instant};

    use conbus_core::event::{Event, Harness, TurnState};
    use serde_json::{Value, json};

    use crate::hook::{self, Normalized};

    #[test]
    fn opencode_plugin_is_written_beside_other_local_plugins() {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = Path::new("/opt/conbus");
        assert!(apply_opencode_plugin(bin, home.path()).expect("write"));
        assert!(!apply_opencode_plugin(bin, home.path()).expect("idempotent"));
        let text = std::fs::read_to_string(opencode_plugin_path(home.path())).expect("read");
        assert!(text.contains(r#"const bin = "/opt/conbus";"#), "{text}");
        assert!(!text.contains("@CONBUS_BIN@"), "the slot was left unfilled");
        assert!(text.contains("session.error"));
        assert!(opencode_plugin_registered(home.path()));
        assert!(opencode_plugin_current(bin, home.path()));
    }

    #[test]
    fn a_plugin_an_older_conbus_wrote_is_replaced_not_kept() {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = Path::new("/Users/you/.local/bin/conbus");
        let path = opencode_plugin_path(home.path());
        fs::create_dir_all(path.parent().expect("plugins")).expect("plugins");
        fs::write(&path, OLDER).expect("the older plugin");
        assert!(
            opencode_plugin_registered(home.path()),
            "it forwards the same events"
        );
        assert!(!opencode_plugin_current(bin, home.path()));

        assert!(apply_opencode_plugin(bin, home.path()).expect("replace"));

        assert!(opencode_plugin_current(bin, home.path()));
        assert_eq!(
            fs::read_to_string(&path).expect("read"),
            opencode_plugin_source(bin)
        );
    }

    // The plugin, run the way OpenCode runs it. Every OpenCode fact below was
    // read in its source at v1.18.31 (identical on `dev` on 2026-09-23), not
    // taken from its docs, which say nothing about order.

    /// The plugin conbus 0.2.3 wrote. Machines keep it until `install --apply`
    /// replaces it, so the hook has to go on reading what it sends.
    const OLDER: &str = include_str!("../../tests/fixtures/opencode_plugin_0.2.3.js");
    /// How the fixture spells the binary it runs.
    const OLDER_BIN: &str = r#""/Users/you/.local/bin/conbus""#;

    /// OpenCode's loader and bus, as far as a plugin can tell. The loader
    /// calls every export of the module as a plugin and refuses one that is
    /// not a function (`getLegacyPlugins`, `src/plugin/index.ts`). The bus
    /// calls each hook's `event` for every event published in the plugin's
    /// directory, in publication order, and never awaits it
    /// (`void hook["event"]?.(…)`, called inline by `publish`). The driver
    /// does the same, then waits for every call to settle so each hook the
    /// plugin spawned has finished writing.
    const DRIVER: &str = r#"import { readFileSync } from "node:fs";
import { pathToFileURL } from "node:url";

const [plugin, script] = process.argv.slice(2);
const loaded = await import(pathToFileURL(plugin).href);
const hooks = [];
for (const [name, init] of Object.entries(loaded)) {
  if (typeof init !== "function") throw new TypeError(`export ${name} is not a plugin`);
  hooks.push(await init({ directory: "/repo" }));
}
const calls = [];
for (const event of JSON.parse(readFileSync(script, "utf8"))) {
  for (const hook of hooks) calls.push(hook.event?.({ event }));
}
await Promise.all(calls);
"#;

    fn current(bin: &Path) -> String {
        opencode_plugin_source(bin)
    }

    fn older(bin: &Path) -> String {
        let spelled = Value::String(bin.display().to_string()).to_string();
        OLDER.replace(OLDER_BIN, &spelled)
    }

    /// node, or a failure that says why. A missing interpreter must not read
    /// as a plugin that behaves.
    fn node() -> &'static str {
        match Command::new("node").arg("--version").output() {
            Ok(out) if out.status.success() => "node",
            Ok(out) => panic!(
                "`node --version` exited {}; these tests load the OpenCode plugin in node",
                out.status
            ),
            Err(err) => panic!(
                "node is not on PATH ({err}). These tests load the OpenCode plugin in node the \
                 way OpenCode loads it; install node (GitHub's macOS runners have v22) rather \
                 than skipping them"
            ),
        }
    }

    /// A stand-in for `conbus hook --harness opencode` that keeps each payload
    /// in a file of its own: the plugin does not wait for one hook before it
    /// spawns the next, so appends to one file could interleave.
    const KEEPS: &str = r#"cat > "$(mktemp '{captured}/payload.XXXXXX')""#;

    /// One that closes its stdin unread and lingers, as a hook does that
    /// exits before reading (`CONBUS_INTERNAL_SESSION`) or dies at exec.
    const HANGS_UP: &str = "exec 0<&-; sleep 0.3";

    fn write_stub(path: &Path, body: &str, captured: &Path) {
        let dir = captured.display().to_string();
        assert!(!dir.contains('\''), "{dir}");
        let script = format!("#!/bin/sh\n{}\n", body.replace("{captured}", &dir));
        crate::test_support::write_executable(path, &script);
    }

    fn settle(mut child: Child, limit: Duration) -> Output {
        let deadline = Instant::now() + limit;
        while child.try_wait().expect("poll node").is_none() {
            if Instant::now() >= deadline {
                let _ = child.kill();
                let out = child.wait_with_output().expect("node output");
                panic!(
                    "the plugin had not settled after {limit:?}:\n{}",
                    String::from_utf8_lossy(&out.stderr)
                );
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        child.wait_with_output().expect("node output")
    }

    /// Every payload a plugin hands the hook for one scripted stream of
    /// OpenCode events, in no particular order: OpenCode does not wait for
    /// one call before it makes the next.
    fn sent(plugin: fn(&Path) -> String, events: &[Value]) -> Vec<Value> {
        drive(plugin, events, KEEPS)
    }

    /// Run a plugin under node against a hook stub, failing if node does.
    fn drive(plugin: fn(&Path) -> String, events: &[Value], stub: &str) -> Vec<Value> {
        let dir = tempfile::tempdir().expect("tempdir");
        let captured = dir.path().join("captured");
        fs::create_dir(&captured).expect("captured");
        let hook = dir.path().join("conbus");
        write_stub(&hook, stub, &captured);
        let source = plugin(&hook);
        assert!(
            source.contains(&hook.display().to_string()),
            "the plugin does not run the stub"
        );
        // OpenCode loads `conbus.js` as an ES module; node has to be told.
        fs::write(dir.path().join("package.json"), r#"{"type":"module"}"#).expect("package");
        fs::write(dir.path().join("conbus.js"), source).expect("plugin");
        fs::write(dir.path().join("driver.mjs"), DRIVER).expect("driver");
        let script = Value::Array(events.to_vec()).to_string();
        fs::write(dir.path().join("events.json"), script).expect("events");

        let child = Command::new(node())
            .args(["driver.mjs", "conbus.js", "events.json"])
            .current_dir(dir.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("start node");
        let out = settle(child, Duration::from_secs(30));
        assert!(
            out.status.success(),
            "the plugin failed under node ({}):\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );

        fs::read_dir(&captured)
            .expect("captured")
            .map(|entry| {
                let text = fs::read_to_string(entry.expect("entry").path()).expect("payload");
                serde_json::from_str(&text).unwrap_or_else(|err| {
                    panic!("the plugin sent something not JSON ({err}): {text}")
                })
            })
            .collect()
    }

    /// One payload, read the way `conbus hook --harness opencode` reads it.
    fn read(payload: &Value) -> (Option<String>, Event) {
        match hook::normalize(&Harness::new(Harness::OPENCODE), &payload.to_string()) {
            Ok(Normalized::Event { observed, .. }) => {
                let observed = *observed;
                let id = observed.vendor_turn_id.map(|id| id.as_str().to_string());
                (id, observed.event)
            }
            other => panic!("the hook would not record {payload}: {other:?}"),
        }
    }

    /// What reaches the bus as `turn_started`, as (message id, prompt), sorted.
    fn turns(sent: &[Value]) -> Vec<(String, String)> {
        let mut turns: Vec<(String, String)> = sent
            .iter()
            .filter_map(|payload| match read(payload) {
                (Some(id), Event::TurnStarted { prompt, .. }) => {
                    Some((id, prompt.as_text().unwrap_or_default().to_string()))
                }
                (None, Event::TurnStarted { .. }) => {
                    panic!("a turn without its message: {payload}")
                }
                _ => None,
            })
            .collect();
        turns.sort();
        turns
    }

    fn turn(id: &str, prompt: &str) -> (String, String) {
        (id.to_string(), prompt.to_string())
    }

    const SESSION: &str = "ses_1";
    const PROMPT: &str = "the checkout test is flaky; find out why";
    const SECOND: &str = "now make it deterministic";
    const TITLE: &str = "Flaky checkout test";
    const REPLY: &str = "It races the payment mock; the fix is to await the mock.";
    const SECOND_REPLY: &str = "Done: the test awaits the mock and passes 200 runs in a row.";

    /// How OpenCode hands a plugin an event: `{ id, type, properties }`.
    fn event(kind: &str, properties: Value) -> Value {
        json!({ "id": format!("evt_{kind}"), "type": kind, "properties": properties })
    }

    fn session_created() -> Value {
        event(
            "session.created",
            json!({ "sessionID": SESSION, "info": {
                "id": SESSION, "slug": "s", "projectID": "p", "directory": "/repo",
                "title": "New session - 2026-09-23T00:00:00.000Z", "version": "1.18.31",
                "time": { "created": 1, "updated": 1 }
            } }),
        )
    }

    fn message(info: Value) -> Value {
        event(
            "message.updated",
            json!({ "sessionID": SESSION, "info": info }),
        )
    }

    fn user_info(id: &str) -> Value {
        json!({
            "id": id, "sessionID": SESSION, "role": "user", "time": { "created": 1 },
            "agent": "build", "model": { "providerID": "anthropic", "modelID": "claude-sonnet" }
        })
    }

    /// A person's message as `createUserMessage` publishes it, before its parts.
    fn user(id: &str) -> Value {
        message(user_info(id))
    }

    /// The same message after an agent step: `SessionSummary.summarize`
    /// writes the step's diff onto it and publishes it again
    /// (anomalyco/opencode#43551).
    fn summarised(id: &str) -> Value {
        let mut info = user_info(id);
        info["summary"] = json!({ "title": TITLE, "diffs": [
            { "file": "tests/checkout.rs", "additions": 3, "deletions": 1 }
        ] });
        message(info)
    }

    fn assistant(id: &str, parent: &str) -> Value {
        message(json!({
            "id": id, "sessionID": SESSION, "role": "assistant", "parentID": parent,
            "time": { "created": 2 }, "modelID": "claude-sonnet", "providerID": "anthropic",
            "mode": "build", "agent": "build", "path": { "cwd": "/repo", "root": "/repo" },
            "cost": 0,
            "tokens": { "input": 0, "output": 0, "reasoning": 0, "cache": { "read": 0, "write": 0 } }
        }))
    }

    fn part(fields: Value) -> Value {
        event(
            "message.part.updated",
            json!({ "sessionID": SESSION, "part": fields, "time": 3 }),
        )
    }

    fn text(message: &str, id: &str, words: &str) -> Value {
        part(json!({
            "id": id, "sessionID": SESSION, "messageID": message, "type": "text", "text": words
        }))
    }

    /// Text OpenCode writes into the person's message itself — what an `@file`
    /// mention read, or a note that a shell command ran — marked `synthetic`.
    fn synthetic(message: &str, id: &str, words: &str) -> Value {
        part(json!({
            "id": id, "sessionID": SESSION, "messageID": message, "type": "text",
            "text": words, "synthetic": true
        }))
    }

    fn ignored(message: &str, id: &str, words: &str) -> Value {
        part(json!({
            "id": id, "sessionID": SESSION, "messageID": message, "type": "text",
            "text": words, "ignored": true
        }))
    }

    fn step(message: &str, kind: &str) -> Value {
        part(json!({
            "id": format!("prt_{message}_{kind}"), "sessionID": SESSION, "messageID": message,
            "type": kind
        }))
    }

    /// One agent step that only runs a tool.
    fn tool_step(message: &str, parent: &str) -> Vec<Value> {
        vec![
            assistant(message, parent),
            step(message, "step-start"),
            part(json!({
                "id": format!("prt_{message}_tool"), "sessionID": SESSION, "messageID": message,
                "type": "tool", "callID": "call_1", "tool": "read",
                "state": { "status": "completed", "input": { "filePath": "tests/checkout.rs" },
                           "output": "…", "title": "tests/checkout.rs", "metadata": {},
                           "time": { "start": 2, "end": 3 } }
            })),
            step(message, "step-finish"),
            assistant(message, parent),
        ]
    }

    /// One agent step that answers in words, the way the processor publishes
    /// it: an empty text part at `text-start`, deltas, and the whole text
    /// again at `text-end`.
    fn reply_step(message: &str, parent: &str, words: &str) -> Vec<Value> {
        let id = format!("prt_{message}_text");
        vec![
            assistant(message, parent),
            step(message, "step-start"),
            text(message, &id, ""),
            event(
                "message.part.delta",
                json!({ "sessionID": SESSION, "messageID": message, "partID": id,
                        "field": "text", "delta": words }),
            ),
            text(message, &id, words),
            step(message, "step-finish"),
            assistant(message, parent),
        ]
    }

    fn idle() -> Value {
        event("session.idle", json!({ "sessionID": SESSION }))
    }

    fn usage_limit() -> Value {
        event(
            "session.error",
            json!({ "sessionID": SESSION, "error": { "name": "APIError", "data": {
                "message": "account_rate_limit", "statusCode": 429, "isRetryable": true,
                "responseBody": "GoUsageLimitError"
            } } }),
        )
    }

    /// Two prompts in one session: the agent answers the first over a tool
    /// step and a reply, the second in one reply, and then a limit stops it.
    fn conversation() -> Vec<Value> {
        [
            vec![
                session_created(),
                user("msg_u1"),
                text("msg_u1", "prt_u1", PROMPT),
            ],
            tool_step("msg_a1", "msg_u1"),
            vec![summarised("msg_u1")],
            reply_step("msg_a2", "msg_u1", REPLY),
            vec![summarised("msg_u1"), summarised("msg_u1"), idle()],
            vec![user("msg_u2"), text("msg_u2", "prt_u2", SECOND)],
            reply_step("msg_a3", "msg_u2", SECOND_REPLY),
            vec![summarised("msg_u2"), usage_limit()],
        ]
        .concat()
    }

    #[test]
    fn a_prompt_is_one_turn_however_often_opencode_publishes_its_message_again() {
        // Two tool-only steps and the end of the loop: the summariser publishes
        // the person's message three more times, with a title the person never
        // wrote.
        let events = [
            vec![
                session_created(),
                user("msg_u1"),
                text("msg_u1", "prt_u1", PROMPT),
            ],
            tool_step("msg_a1", "msg_u1"),
            vec![summarised("msg_u1")],
            tool_step("msg_a2", "msg_u1"),
            vec![summarised("msg_u1"), summarised("msg_u1")],
            vec![user("msg_u2"), text("msg_u2", "prt_u2", SECOND)],
            vec![summarised("msg_u2")],
        ]
        .concat();

        assert_eq!(
            turns(&sent(current, &events)),
            [turn("msg_u1", PROMPT), turn("msg_u2", SECOND)]
        );
    }

    #[test]
    fn the_agents_reply_is_never_a_prompt() {
        let events = [
            vec![user("msg_u1"), text("msg_u1", "prt_u1", PROMPT)],
            reply_step("msg_a1", "msg_u1", REPLY),
            vec![summarised("msg_u1")],
            reply_step("msg_a2", "msg_u1", SECOND_REPLY),
        ]
        .concat();

        let sent = sent(current, &events);

        assert_eq!(turns(&sent), [turn("msg_u1", PROMPT)]);
        for payload in &sent {
            let text = payload.to_string();
            assert!(
                !text.contains(REPLY) && !text.contains(SECOND_REPLY),
                "the agent's words reached the hook: {text}"
            );
        }
    }

    #[test]
    fn text_opencode_writes_into_the_message_is_not_the_persons_prompt() {
        // OpenCode's own timeline takes the first text part that is neither
        // `synthetic` nor `ignored` as what the person said
        // (`dialog-timeline.tsx`). A message with no such part — a shell
        // command the person ran, say — is not a prompt at all.
        let mention =
            "Called the Read tool with the following input: {\"filePath\":\"src/pay.rs\"}";
        let events = vec![
            user("msg_u1"),
            synthetic("msg_u1", "prt_u1a", mention),
            text("msg_u1", "prt_u1b", PROMPT),
            synthetic("msg_u1", "prt_u1c", "fn pay() {}"),
            user("msg_u2"),
            synthetic(
                "msg_u2",
                "prt_u2",
                "The following tool was executed by the user",
            ),
            user("msg_u3"),
            ignored("msg_u3", "prt_u3", "left out of the model's context"),
        ];

        assert_eq!(turns(&sent(current, &events)), [turn("msg_u1", PROMPT)]);
    }

    #[test]
    fn a_part_published_before_its_message_waits_to_learn_whose_it_is() {
        // OpenCode 1.18.31 publishes a message before its parts. Should that
        // ever flip, a part has to wait for its message rather than be guessed.
        let events = vec![
            text("msg_u1", "prt_u1", PROMPT),
            text("msg_a1", "prt_a1", REPLY),
            user("msg_u1"),
            assistant("msg_a1", "msg_u1"),
            summarised("msg_u1"),
        ];

        let sent = sent(current, &events);

        assert_eq!(turns(&sent), [turn("msg_u1", PROMPT)]);
        assert!(
            sent.iter()
                .all(|payload| !payload.to_string().contains(REPLY)),
            "{sent:?}"
        );
    }

    #[test]
    fn parts_whose_message_never_arrives_are_not_kept_without_limit() {
        // A hundred parts for messages that are never announced, then the
        // first and the last of them are. A plugin still holding the first
        // is holding every part it was ever handed.
        const PARTS: usize = 100;
        let id = |n: usize| format!("msg_x{n:03}");
        let events: Vec<Value> = (0..PARTS)
            .map(|n| text(&id(n), &format!("prt_x{n:03}"), &format!("prompt {n}")))
            .chain([user(&id(0)), user(&id(PARTS - 1))])
            .collect();

        assert_eq!(
            turns(&sent(current, &events)),
            [turn(&id(PARTS - 1), &format!("prompt {}", PARTS - 1))]
        );
    }

    #[test]
    fn a_hook_that_hangs_up_unread_does_not_take_opencode_down() {
        // Node raises EPIPE on a child's stdin when the child closes it before
        // the payload is through, and an unheard stream error is an uncaught
        // exception — in OpenCode's own process. A prompt larger than the
        // pipe buffer meeting a hook that exits before reading is enough.
        let long = "x".repeat(1 << 20);
        let events = vec![user("msg_u1"), text("msg_u1", "prt_u1", &long)];

        drive(current, &events, HANGS_UP);
    }

    #[test]
    fn the_bus_hears_the_session_once_each_prompt_once_and_the_limit() {
        let events: Vec<Event> = sent(current, &conversation())
            .iter()
            .map(|payload| read(payload).1)
            .collect();

        let count = |wanted: fn(&Event) -> bool| events.iter().filter(|e| wanted(e)).count();
        let starts = count(|e| matches!(e, Event::SessionCandidate { .. }));
        let prompts = count(|e| matches!(e, Event::TurnStarted { .. }));
        let limits = count(|e| {
            matches!(
                e,
                Event::TurnEnded {
                    state: TurnState::InterruptedQuota { .. },
                    ..
                }
            )
        });
        assert_eq!(
            (starts, prompts, limits, events.len()),
            (1, 2, 1, 4),
            "{events:?}"
        );
    }

    #[test]
    fn the_session_start_and_the_error_are_sent_exactly_as_before() {
        let lifecycle = |plugin: fn(&Path) -> String| {
            let mut payloads: Vec<Value> = sent(plugin, &conversation())
                .into_iter()
                .filter(|p| {
                    p["hook_event_name"]
                        .as_str()
                        .is_some_and(|name| name.starts_with("session."))
                })
                .collect();
            payloads.sort_by_key(|p| p["hook_event_name"].to_string());
            payloads
        };

        let before = lifecycle(older);

        assert_eq!(before.len(), 2, "{before:?}");
        assert_eq!(lifecycle(current), before);
    }

    #[test]
    fn every_payload_the_older_plugin_sends_is_still_read() {
        let payloads = sent(older, &conversation());

        assert!(payloads.len() > 4, "{payloads:?}");
        for payload in &payloads {
            read(payload);
        }
    }
}
