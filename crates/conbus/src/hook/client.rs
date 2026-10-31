//! The thin client the vendor CLI actually runs.
//!
//! Two rules govern everything here. It must never fail the turn: a non-zero
//! exit from `UserPromptSubmit` blocks the user's prompt, and losing a log line
//! is cheaper than losing the turn. And it must never mirror our own background
//! sessions, because the hooks are global and the summariser we spawn would
//! otherwise log itself.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;

use conbus_core::event::{Entrypoint, Harness};
use conbus_core::paths::Paths;
use conbus_core::registry::SessionKey;

use crate::drops::Dropped;
use crate::hook::{self, Normalized};
use crate::wire::{Request, Response};

/// What the hook says on stdout when it has nothing to add, to Claude Code.
///
/// It is JSON because the vendor's hook contract reads stdout as JSON control
/// output; the two fields say "carry on" and "keep this out of the transcript".
/// On every path that does not brief — success or failure — this is the whole
/// of the output.
///
/// It exists because writing *nothing* is not the same as saying nothing. A
/// zero-byte stdout on a context-injecting event was observed to reach the API
/// as an empty text block carrying `cache_control`, which the API rejects —
/// taking the user's session down with it. See the quirk note in HANDOFF.md.
///
/// Codex is told nothing differently: see [`silent_reply`].
pub const SILENT_OK: &str = r#"{"continue":true,"suppressOutput":true}"#;

/// The reply that says nothing, in the dialect the harness accepts.
///
/// Codex 0.152.1 fails a `PreToolUse` hook that says `continue` or
/// `suppressOutput`, and rejects `suppressOutput` on `PostToolUse`. The first
/// session run with trusted hooks showed "hook (failed)" twice per tool call
/// while every record had already landed — the recorder was right and the
/// person saw it failing. Codex's reference lists the fields each event takes;
/// the empty object is the one reply all of them take, and nothing on the
/// Codex side has ever needed the non-empty one.
pub fn silent_reply(harness: &str) -> &'static str {
    match harness {
        Harness::CODEX => "{}",
        _ => SILENT_OK,
    }
}

/// What to print when the record did not land.
///
/// Inference hooks must still allow — a Claude `continue` object is not a
/// verdict, and a deny would block the Chat window we are attaching.
pub fn failure_reply(harness: &str, payload: &str) -> &'static str {
    if hook::inference::looks_like(payload) {
        hook::inference::ALLOW
    } else {
        silent_reply(harness)
    }
}

/// What this run declares about itself, read from the hook child's own
/// environment exactly as `CONBUS_BRIEFING` is. A probe sets it so its hops
/// are not counted as the router deciding for itself; nothing else reads it,
/// and it cannot change the reply or the exit code.
fn declared_provenance() -> Option<conbus_core::event::HopProvenance> {
    std::env::var_os("CONBUS_PROVENANCE")
        .filter(|value| !value.is_empty())
        .map(|_| conbus_core::event::HopProvenance::DeclaredSynthetic)
}

/// How long the hook will wait on the daemon before giving up on it.
pub const REPLY_DEADLINE: std::time::Duration = std::time::Duration::from_millis(500);

/// The environment marker that tells a hook it is inside a session we started.
pub const INTERNAL_MARKER: &str = "CONBUS_INTERNAL_SESSION";

/// The vendor's version, if it exposes one to its hook children.
///
/// `CLAUDE_CODE_VERSION` exists as a name inside the Claude Code binary but is
/// not exported to a hook child on 2.1.241, and Codex has no equivalent at all,
/// so this is `None` in practice today. It is read anyway: the moment a vendor
/// starts setting it the log gains a version stamp with no further work, and
/// reading an absent variable costs nothing.
fn vendor_version(harness: &str) -> Option<String> {
    let name = match harness {
        Harness::CLAUDE_CODE => "CLAUDE_CODE_VERSION",
        Harness::CODEX => "CODEX_VERSION",
        _ => return None,
    };
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// The top-level field names of a payload, for a diagnostic about one we could
/// not read.
///
/// Names only, never values. A payload's keys are its schema and say who sent
/// it — which is the question an unmapped event raises, since Cursor, Copilot
/// CLI and Devin CLI all read Claude Code's settings file and will invoke this
/// hook under Claude Code's name. Its values are somebody's conversation, and
/// the drop log is not a place to put one.
fn field_names(payload: &str) -> String {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(payload)
    else {
        return "unreadable".to_string();
    };
    let mut names: Vec<&str> = fields.keys().map(String::as_str).collect();
    names.sort_unstable();
    names.join(",")
}

/// The front end this session runs under, when the harness tells its children.
///
/// Read from the environment because the payload has no field for it: Claude
/// Code sets `CLAUDE_CODE_ENTRYPOINT` in every hook child, and it is the only
/// durable answer to whether a person is sitting there. Codex sets no
/// equivalent, so its sessions are an honest unknown.
fn entrypoint(harness: &str) -> Option<Entrypoint> {
    let name = match harness {
        Harness::CLAUDE_CODE => "CLAUDE_CODE_ENTRYPOINT",
        _ => return None,
    };
    std::env::var(name)
        .ok()
        .filter(|value| !value.is_empty())
        .map(Entrypoint::new)
}

/// Whether the person has switched briefings off: `CONBUS_BRIEFING=off`
/// (or `0`, `false`, `no`) in the environment the vendor runs hooks in.
///
/// The one lever toward silence that does not also stop the recording. Read
/// here and carried on the wire, so the daemon neither injects nor writes a
/// `briefed` record for a briefing nobody received.
fn briefing_enabled() -> bool {
    !std::env::var("CONBUS_BRIEFING").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "off" | "0" | "false" | "no"
        )
    })
}

/// The line to print on stdout, and whether it carries anything.
///
/// A hook always says something well-formed; the only question is whether it
/// also hands the model a briefing.
pub type Reply = String;

/// Build the reply that carries a briefing.
///
/// Serialised rather than formatted, because the context is arbitrary text from
/// somebody's conversation: a quote mark or a newline written by hand would
/// otherwise produce stdout the vendor cannot parse, and unparseable stdout on
/// `UserPromptSubmit` is not dropped — it is injected verbatim as plain hook
/// output, which is a silently different payload rather than a failure.
///
/// `hookEventName` is the firing event, carried from the payload rather than
/// written here. The vendor throws away the whole hook result when it does not
/// match, without blocking the turn, and reports that failure only once per
/// session — so a name that drifts would be invisible after its first turn.
pub fn briefing_reply(harness: &str, hook_event_name: &str, context: &str, message: &str) -> Reply {
    // Per-field cap, UTF-16 units, inclusive. Past it the vendor persists the
    // text and the model gets a preview — silent degradation. Refuse here
    // rather than ship a pack that looks like it landed.
    if conbus_core::pack::units(context) > conbus_core::pack::VENDOR_CAP_UNITS
        || conbus_core::pack::units(message) > conbus_core::pack::VENDOR_CAP_UNITS
    {
        return silent_reply(harness).to_string();
    }
    if harness == Harness::CURSOR {
        // Official Cursor inject is sessionStart `additional_context`.
        // hookSpecificOutput is Claude's dialect and is ignored here.
        return serde_json::json!({
            "continue": true,
            "suppressOutput": true,
            "additional_context": context,
        })
        .to_string();
    }
    let mut reply = serde_json::json!({
        "systemMessage": message,
        "hookSpecificOutput": {
            "hookEventName": hook_event_name,
            "additionalContext": context,
        },
    });
    // Claude Code's two control fields. `suppressOutput` is kept even though
    // it is inert in 2.1.241 — its single runtime read is unreachable for a
    // hook that emits JSON — because it is the documented way to say "keep
    // this out of the transcript" and costs nothing if a later build wires it
    // up. It does not suppress the injection: nothing on that path reads it.
    // Codex is not sent either: it fails a hook over fields it does not take
    // (see `silent_reply`), and its reference accepts a briefing without them.
    if harness != Harness::CODEX {
        reply["continue"] = serde_json::Value::Bool(true);
        reply["suppressOutput"] = serde_json::Value::Bool(true);
    }
    reply.to_string()
}

/// Mirror one hook payload. Returns the line to print, or a diagnostic when the
/// record did not land; the caller reports it, prints the silent reply, and
/// still exits zero.
pub fn run(harness: &str, stdin: &mut impl Read) -> Result<Reply, Dropped> {
    run_in(Paths::resolve().ok().as_ref(), harness, stdin)
}

/// As [`run`], with the data root named rather than read from the
/// environment: the Inference listener resolves it once, and a test hands
/// over a root of its own instead of reaching the person's recorder.
pub fn run_in(
    paths: Option<&Paths>,
    harness: &str,
    stdin: &mut impl Read,
) -> Result<Reply, Dropped> {
    if std::env::var_os(INTERNAL_MARKER).is_some_and(|value| !value.is_empty()) {
        return Ok(silent_reply(harness).to_string());
    }

    let mut payload = String::new();
    stdin
        .read_to_string(&mut payload)
        .map_err(|err| format!("could not read the hook payload: {err}"))?;

    let inference = hook::inference::looks_like(&payload);
    let normalized = hook::normalize(&Harness::new(harness), &payload).map_err(|err| err.0)?;
    let (key, hook_event_name, observed) = match normalized {
        Normalized::Event {
            key: k,
            hook_event_name: name,
            observed: o,
        } => (k, name, *o),
        // A decision, so nothing to say. Inference frames still need the
        // archival verdict — Claude hook JSON would fail Anthropic's parse.
        Normalized::Ignored { .. } => {
            return Ok(if inference {
                hook::inference::ALLOW.to_string()
            } else {
                silent_reply(harness).to_string()
            });
        }
        // A surprise. The vendor grew an event this adapter has never seen, and
        // it is being dropped — which is the right thing to do and the wrong
        // thing to do quietly.
        Normalized::Unknown { hook_event_name } => {
            return Err(format!(
                "no mapping for hook event {hook_event_name:?} (as {harness}, fields: {})",
                field_names(&payload)
            )
            .into());
        }
    };
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    if grok_echo(harness, &key, &hook_event_name, home.as_deref()) {
        return Ok(silent_reply(harness).to_string());
    }
    named_session(&key, &hook_event_name)?;
    let paths = paths.ok_or_else(|| "no home directory to resolve".to_string())?;
    if let Some(problem) = paths.socket_problem() {
        return Err(Dropped::new(problem));
    }
    if key.harness.as_str() == Harness::CURSOR
        || key.harness.as_str() == Harness::AGY
        || key.harness.as_str() == Harness::OPENCODE
        || key.harness.as_str() == Harness::GROK
        || key.harness.as_str() == Harness::CLAUDE_CHAT
    {
        crate::quota::observe_hook_keys(paths, key.harness.as_str(), &payload);
    }
    let socket = paths.socket();
    let request = Request {
        protocol_version: Some(crate::wire::PROTOCOL_VERSION),
        harness: key.harness.clone(),
        harness_version: vendor_version(harness),
        vendor_session_id: key.vendor_session_id,
        vendor_turn_id: observed.vendor_turn_id,
        vendor_agent_id: observed.vendor_agent_id,
        workspace: observed.workspace,
        entrypoint: if inference {
            Some(Entrypoint::new("claude_ai_chat"))
        } else {
            entrypoint(harness)
        },
        // Inference hooks only take allow/deny — a briefing object fails
        // the Chat turn Anthropic is holding. Cursor briefs on sessionStart
        // via additional_context; beforeSubmitPrompt still asks so a first
        // prompt can carry the same pack if that event later grows a field.
        brief: briefing_enabled() && !inference,
        provenance: declared_provenance(),
        event: observed.event,
    };
    let result = match send_once(harness, &socket, &request, &hook_event_name) {
        Err(dropped) if write_failed(&dropped.reason) => {
            // The daemon is single-threaded. A write that hits a closing
            // peer is usually the previous hook still being served; wait
            // once, then try a new connection. An empty reply after a
            // successful write is not retried: the daemon may already
            // have appended.
            std::thread::sleep(std::time::Duration::from_millis(40));
            send_once(harness, &socket, &request, &hook_event_name)
        }
        other => other,
    };
    match result {
        Ok(_) if inference => Ok(hook::inference::ALLOW.to_string()),
        other => other,
    }
}

/// Whether this call is Grok running somebody else's registration of the hook,
/// for an event Grok's own registration delivers too.
///
/// Grok's hook docs say it reads Claude Code's `.claude/settings.json` and
/// Cursor's `.cursor/hooks.json` beside its own `~/.grok/hooks/*.json`, and it
/// runs every hook it finds. The harness is named from the payload
/// (`hook::normalize`), so the copy that came in through Claude Code's
/// registration was stamped `grok` as well: from 2026-09-06, the day `install`
/// began writing Grok's own file, every Grok event reached the log twice — one
/// conversation, the same ids and body, 30–330 ms apart.
///
/// The copy kept is the one Grok runs from its own directory, which it trusts
/// without asking; its Claude-compatible hooks have shipped loaded and never
/// dispatched (1.0.3, `xai-org/plugin-marketplace#236`). The echo stands down
/// only while that file registers the recorder for this event through a binary
/// that is there. Without it the echo is the only copy, and answering it
/// silently would lose the event.
fn grok_echo(
    invoked_as: &str,
    key: &SessionKey,
    hook_event_name: &str,
    home: Option<&std::path::Path>,
) -> bool {
    key.harness.as_str() == Harness::GROK
        && invoked_as != Harness::GROK
        && home.is_some_and(|home| crate::install::grok_registers(home, hook_event_name))
}

/// Refuse a payload that names no session, before any socket is opened
/// (`hook::NO_SESSION`). Its own function so a test can hold the refusal
/// without a daemon — and without ever reaching someone's real one.
fn named_session(key: &SessionKey, hook_event_name: &str) -> Result<(), Dropped> {
    if key.vendor_session_id.trim().is_empty() {
        return Err(Dropped::new(hook::NO_SESSION)
            .with_detail(format!("{hook_event_name} as {}", key.harness.as_str())));
    }
    Ok(())
}

fn connect(socket: &std::path::Path) -> Result<UnixStream, Dropped> {
    let stream = UnixStream::connect(socket)
        .map_err(|err| format!("no daemon on {}: {err}", socket.display()))?;
    // A hook must never fail the turn, and it must not delay it either. Without
    // these the read blocks forever on a daemon that is up but wedged, and the
    // vendor's own timeout becomes the user's wait: 30 seconds on Claude Code's
    // `UserPromptSubmit`, with the prompt stalled the whole time.
    //
    // Half a second, because the tightest vendor timeout observed is Codex's
    // own default of **one second** on `SessionEnd` — not something conbus
    // registers, something Codex imposes on session teardown. The daemon's work
    // per request is a map lookup and an append, so anything approaching this
    // is a fault rather than a slow path, and losing that record is cheaper
    // than making somebody wait for it.
    for limit in [
        stream.set_read_timeout(Some(REPLY_DEADLINE)),
        stream.set_write_timeout(Some(REPLY_DEADLINE)),
    ] {
        limit.map_err(|err| format!("could not bound the daemon call: {err}"))?;
    }
    Ok(stream)
}

fn send_once(
    harness: &str,
    socket: &std::path::Path,
    request: &Request,
    hook_event_name: &str,
) -> Result<Reply, Dropped> {
    send(harness, connect(socket)?, request, hook_event_name)
}

/// Retry is safe only when the write itself failed. The daemon may already
/// have appended if we got as far as an empty read.
fn write_failed(reason: &str) -> bool {
    reason.contains("could not send the record:")
        && (reason.contains("Broken pipe")
            || reason.contains("Socket is not connected")
            || reason.contains("Connection reset")
            || reason.contains("Resource temporarily unavailable"))
}

fn send(
    harness: &str,
    mut stream: UnixStream,
    request: &Request,
    hook_event_name: &str,
) -> Result<Reply, Dropped> {
    let line = serde_json::to_string(request).map_err(|err| err.to_string())?;
    writeln!(stream, "{line}").map_err(|err| format!("could not send the record: {err}"))?;
    stream
        .flush()
        .map_err(|err| format!("could not send the record: {err}"))?;

    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|err| format!("no reply from the daemon: {err}"))?;

    match serde_json::from_str::<Response>(reply.trim()) {
        Ok(Response::Recorded { .. } | Response::Deferred) => {
            Ok(silent_reply(harness).to_string())
        }
        Ok(Response::Briefed {
            context, message, ..
        }) => Ok(briefing_reply(harness, hook_event_name, &context, &message)),
        Ok(Response::Failed { error }) => {
            Err(format!("the daemon refused the record: {error}").into())
        }
        // Not written, and the caller has to hear about it. An orphan is the
        // normal shape of a daemon restart mid-turn: the session is unbound, so
        // the rest of that turn belongs to no conversation. Reporting it is the
        // difference between a known gap and a turn that quietly disappears.
        // The reason is the throttle key and stays fixed; the detail names
        // the event and the session — identifiers the log already holds in
        // full — so a ghost session's end can be told from a lost tool call
        // when the drop log is read back.
        Ok(Response::Orphaned) => Err(Dropped::new(
            "no conversation for this session; the daemon has no binding for it              (a restart mid-turn looks exactly like this)",
        )
        .with_detail(format!(
            "{hook_event_name} for session {}",
            request.vendor_session_id.chars().take(8).collect::<String>()
        ))),
        Ok(Response::Withheld) => {
            Err("retention withheld this record from disk".to_string().into())
        }
        Err(err) => Err(format!("unreadable reply {reply:?}: {err}").into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unmapped_payload_is_described_by_its_schema_and_never_its_content() {
        // The diagnostic has to be enough to identify the sender — three
        // products read Claude Code's settings file and will call this hook
        // under Claude Code's name — and it goes into a durable drop log, so it
        // must not carry a line of anybody's conversation.
        let described = field_names(
            r#"{"hook_event_name":"beforeSubmitPrompt","prompt":"the secret plan","cwd":"/repo"}"#,
        );

        assert_eq!(described, "cwd,hook_event_name,prompt");
        assert!(!described.contains("secret"));
    }

    #[test]
    fn a_payload_that_names_no_session_is_dropped_before_it_reaches_the_daemon() {
        // Refused before a socket is opened, so it cannot mint the one
        // conversation every nameless session would otherwise share.
        let nameless = r#"{"session_id":"","hook_event_name":"UserPromptSubmit","prompt":"hi","prompt_id":"p1"}"#;
        let named = r#"{"session_id":"s-1","hook_event_name":"UserPromptSubmit","prompt":"hi","prompt_id":"p1"}"#;
        let key = |json: &str| match hook::normalize(&Harness::new(Harness::CLAUDE_CODE), json)
            .expect("normalise")
        {
            Normalized::Event { key, .. } => key,
            other => panic!("{other:?}"),
        };

        let dropped = named_session(&key(nameless), "UserPromptSubmit")
            .expect_err("an empty session id must not be sent");
        assert_eq!(dropped.reason, hook::NO_SESSION);
        assert_eq!(
            dropped.detail.as_deref(),
            Some("UserPromptSubmit as claude_code")
        );
        assert!(named_session(&key(named), "UserPromptSubmit").is_ok());
    }

    /// A home where `install` wrote Grok's own registration, naming a binary
    /// that is there.
    fn home_with_groks_own_registration() -> tempfile::TempDir {
        let home = tempfile::tempdir().expect("tempdir");
        let binary = home.path().join("bin").join("conbus");
        std::fs::create_dir_all(binary.parent().expect("bin")).expect("bin");
        std::fs::write(&binary, "").expect("binary");
        let target = crate::install::targets(&binary, home.path())
            .into_iter()
            .find(|t| t.harness == Harness::GROK)
            .expect("grok target");
        crate::install::apply(&crate::install::plan(target).expect("plan")).expect("write");
        home
    }

    fn key_of(payload: &str) -> SessionKey {
        match hook::normalize(&Harness::new(Harness::CLAUDE_CODE), payload).expect("normalise") {
            Normalized::Event { key, .. } => key,
            other => panic!("{other:?}"),
        }
    }

    const GROK_PROMPT: &str = r#"{"hookEventName":"user_prompt_submit",
        "hook_event_name":"UserPromptSubmit","sessionId":"grok-1","prompt":"hi"}"#;

    #[test]
    fn grok_running_another_vendors_registration_is_an_echo_of_its_own() {
        let home = home_with_groks_own_registration();
        let grok = key_of(GROK_PROMPT);

        assert!(grok_echo(
            Harness::CLAUDE_CODE,
            &grok,
            "UserPromptSubmit",
            Some(home.path())
        ));
        assert!(
            grok_echo(
                Harness::CURSOR,
                &grok,
                "UserPromptSubmit",
                Some(home.path())
            ),
            "Grok reads Cursor's hooks file too"
        );
        assert!(
            !grok_echo(Harness::GROK, &grok, "UserPromptSubmit", Some(home.path())),
            "the copy from Grok's own file is the one kept"
        );
    }

    #[test]
    fn an_echo_with_no_copy_of_its_own_to_defer_to_is_recorded() {
        let grok = key_of(GROK_PROMPT);
        let bare = tempfile::tempdir().expect("tempdir");
        let home = home_with_groks_own_registration();

        assert!(
            !grok_echo(
                Harness::CLAUDE_CODE,
                &grok,
                "UserPromptSubmit",
                Some(bare.path())
            ),
            "Grok installed after conbus: Claude Code's registration is all it runs"
        );
        assert!(!grok_echo(
            Harness::CLAUDE_CODE,
            &grok,
            "UserPromptSubmit",
            None
        ));
        assert!(
            !grok_echo(
                Harness::CLAUDE_CODE,
                &grok,
                "Notification",
                Some(home.path())
            ),
            "an event only Claude Code's registration carries"
        );
    }

    #[test]
    fn only_a_grok_payload_is_ever_an_echo() {
        let home = home_with_groks_own_registration();
        let claude =
            key_of(r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi"}"#);
        let cursor = key_of(
            r#"{"hook_event_name":"beforeSubmitPrompt","cursor_version":"3.19.13",
                "conversation_id":"c","prompt":"hi"}"#,
        );

        for (key, event) in [(claude, "UserPromptSubmit"), (cursor, "beforeSubmitPrompt")] {
            assert!(
                !grok_echo(Harness::CLAUDE_CODE, &key, event, Some(home.path())),
                "{key:?}"
            );
        }
    }

    #[test]
    fn a_payload_that_is_not_an_object_says_so_rather_than_guessing() {
        assert_eq!(field_names("[1,2,3]"), "unreadable");
        assert_eq!(field_names("not json"), "unreadable");
    }

    #[test]
    fn codex_is_never_sent_a_field_it_rejects() {
        // Seen in the Codex 0.152.1 TUI the first time its hooks were trusted:
        // "PreToolUse hook (failed) — error: PreToolUse hook returned
        // unsupported suppressOutput", and the same on PostToolUse, on every
        // tool call. The record had already landed; what the person saw was
        // a hook failing twice per tool call. Codex's reference is explicit:
        // on PreToolUse `continue`, `stopReason` and `suppressOutput` fail the
        // hook, and `suppressOutput` is unsupported on PostToolUse. An empty
        // object is the one reply every Codex event accepts.
        assert_eq!(silent_reply(Harness::CODEX), "{}");

        let brief = briefing_reply(Harness::CODEX, "UserPromptSubmit", "the pack", "one line");
        assert!(!brief.contains("suppressOutput"), "{brief}");
        assert!(!brief.contains("\"continue\""), "{brief}");
        assert!(
            brief.contains("\"additionalContext\":\"the pack\""),
            "{brief}"
        );
        assert!(brief.contains("\"systemMessage\":\"one line\""), "{brief}");
    }

    #[test]
    fn an_inference_frame_still_allows_when_the_record_fails() {
        let frame = include_str!("../../tests/fixtures/claude_inference_hook_prompt.json");
        assert_eq!(
            failure_reply(Harness::CLAUDE_CODE, frame),
            hook::inference::ALLOW
        );
        assert_eq!(
            failure_reply(Harness::CLAUDE_CODE, r#"{"session_id":"s"}"#),
            SILENT_OK
        );
    }

    #[test]
    fn claude_code_keeps_the_reply_that_kept_its_sessions_up() {
        // The other vendor is the reason the silent reply is not empty: a
        // zero-byte stdout on a context-injecting event took a real session
        // down. Codex's rule must not leak into Claude Code's reply.
        assert_eq!(silent_reply(Harness::CLAUDE_CODE), SILENT_OK);
        assert!(
            briefing_reply(Harness::CLAUDE_CODE, "UserPromptSubmit", "c", "m")
                .contains("\"suppressOutput\":true")
        );
    }

    #[test]
    fn a_pack_over_the_vendor_cap_is_refused_not_degraded() {
        let over = "x".repeat(conbus_core::pack::VENDOR_CAP_UNITS + 1);
        assert_eq!(
            briefing_reply(Harness::CLAUDE_CODE, "UserPromptSubmit", &over, "m"),
            SILENT_OK
        );
        assert_eq!(
            briefing_reply(Harness::CODEX, "UserPromptSubmit", "c", &over),
            "{}"
        );
    }

    #[test]
    fn cursor_briefing_uses_additional_context_not_claude_output() {
        let brief = briefing_reply(Harness::CURSOR, "sessionStart", "the pack", "one line");
        assert!(
            brief.contains("\"additional_context\":\"the pack\""),
            "{brief}"
        );
        assert!(!brief.contains("hookSpecificOutput"), "{brief}");
        assert!(!brief.contains("additionalContext"), "{brief}");
        assert!(brief.contains("\"continue\":true"), "{brief}");
    }

    #[test]
    fn a_failed_write_is_retried_an_empty_reply_is_not() {
        assert!(write_failed(
            "could not send the record: Broken pipe (os error 32)"
        ));
        assert!(write_failed(
            "could not send the record: Socket is not connected (os error 57)"
        ));
        assert!(!write_failed(
            "unreadable reply \"\": EOF while parsing a value at line 1 column 0"
        ));
        assert!(!write_failed("no daemon on /tmp/x"));
        assert!(crate::drops::is_transient(
            "unreadable reply \"\": EOF while parsing a value at line 1 column 0"
        ));
    }
}
