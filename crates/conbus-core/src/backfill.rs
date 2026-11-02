//! Recovering conversations the recorder was not running for.
//!
//! The one place the plan allows a vendor session file to be parsed. The live
//! path never may: Claude's docs call the transcript format internal and it
//! changes between releases. Backfill is different because the failure costs
//! differ — a live-path failure loses the conversation happening now, while a
//! backfill failure only leaves the corpus thinner than it could have been. So
//! this parser is allowed to be wrong, and is built to be wrong quietly: it
//! reads an allow-set of three line shapes, counts everything else, and treats
//! partial success as success.
//!
//! Everything it recovers is stamped `backfilled_from`, because a reconstructed
//! record is a weaker fact than a mirrored one and a reader that cannot tell
//! them apart will over-trust the log.

use std::collections::{BTreeMap, HashSet};

use serde::Deserialize;
use serde_json::Value;

use crate::clock::Timestamp;
use crate::event::{Author, Body, Entrypoint, Event, Harness, Source, TurnState, VendorTurnId};
use crate::ids::BusId;
use crate::record::Entry;

/// Names the shape this parser read, so a record can say where it came from.
pub const CLAUDE_TRANSCRIPT: &str = "claude_transcript";

/// What one transcript yielded.
#[derive(Debug, Default)]
pub struct Recovered {
    /// The vendor's session id: the file's name for a transcript, the thread's
    /// own id for a Codex thread.
    pub vendor_session_id: Option<String>,
    /// The checkout the conversation happened in, from the lines themselves.
    pub workspace: Option<String>,
    /// The front end it ran under, from the lines themselves. The live hook
    /// path reads this from the environment; the transcript writes it on every
    /// line, so history gets it too.
    pub entrypoint: Option<Entrypoint>,
    pub entries: Vec<Entry>,
    /// Lines the parser did not read, counted by why. A histogram rather than a
    /// list: a transcript is mostly tool traffic and attachments, and a
    /// per-line report would be longer than the conversation it describes.
    pub skipped: BTreeMap<String, u64>,
}

impl Recovered {
    fn skip(&mut self, reason: impl Into<String>) {
        *self.skipped.entry(reason.into()).or_default() += 1;
    }
}

/// One line of a Claude Code transcript, read tolerantly.
#[derive(Deserialize)]
struct Line {
    #[serde(default)]
    r#type: Option<String>,
    #[serde(rename = "sessionId", default)]
    session_id: Option<String>,
    #[serde(default)]
    timestamp: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(rename = "promptId", default)]
    prompt_id: Option<String>,
    #[serde(rename = "promptSource", default)]
    prompt_source: Option<String>,
    #[serde(rename = "isSidechain", default)]
    is_sidechain: Option<bool>,
    #[serde(rename = "isMeta", default)]
    is_meta: Option<bool>,
    #[serde(rename = "isCompactSummary", default)]
    is_compact_summary: Option<bool>,
    #[serde(rename = "isVisibleInTranscriptOnly", default)]
    is_visible_in_transcript_only: Option<bool>,
    #[serde(rename = "queueTranscriptOnly", default)]
    queue_transcript_only: Option<bool>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    entrypoint: Option<String>,
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<serde_json::Value>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    stop_reason: Option<String>,
}

/// What the model's last line said about whether it had stopped.
///
/// Measured on real transcripts: the model's lines carry `stop_reason` on all
/// but 89 of 242,992, and every line of a tool call says `tool_use`. It is the
/// only thing in a transcript that tells a finished turn from one still running
/// or cut off, which matters because an ending cannot be taken back.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ModelStop {
    /// It finished what it had to say.
    Stopped,
    /// It had not finished: it was on a tool call, or something cut it short.
    /// Live, `max_tokens` reaches the recorder as `StopFailure`, not `Complete`.
    Working,
    /// The line does not say. Builds that wrote no `stop_reason`, or a line
    /// caught before the vendor knew.
    Unsaid,
}

impl ModelStop {
    fn of(stop_reason: Option<&str>) -> Self {
        match stop_reason {
            Some("end_turn") => ModelStop::Stopped,
            Some(_) => ModelStop::Working,
            None => ModelStop::Unsaid,
        }
    }
}

/// The model the vendor names on an assistant line it wrote itself: an API
/// error, a usage limit, "No response requested." Live, a failure reaches the
/// recorder as `StopFailure`, not as a reply.
const VENDOR_WROTE_THIS: &str = "<synthetic>";

/// What a `user` line with text is, in the terms of the live path: whether the
/// prompt hook would have fired for it.
///
/// The vendor's flags decide before the words do, because the words mislead.
/// Against the live log, joined by `promptId` over 2,094 transcripts: 186
/// prompts that open on `<system-reminder>` and 1,399 `<task-notification>`
/// lines fired the hook with the same text, and 522 `isMeta` lines did too.
/// What those meta lines have, and the 5 that did not fire have not, is a
/// `promptSource`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum UserLine {
    /// A prompt: a turn starts here.
    Prompt,
    /// Only a turn if the model goes on to answer it. A typed slash command
    /// looks the same whether the model will run it or the vendor will answer
    /// it locally, and an image with no words has nothing to go on at all.
    Tentative,
    /// The person stopped the turn in progress.
    Interrupt,
    /// A local command's output: the command before it never reached the model.
    LocalOutput,
    /// Written for the vendor's own use, and not a turn or the end of one.
    Bookkeeping(&'static str),
}

fn read_user_line(line: &Line, text: &str) -> UserLine {
    let set = |flag: Option<bool>| flag == Some(true);
    if set(line.is_compact_summary) || set(line.is_visible_in_transcript_only) {
        return UserLine::Bookkeeping("compaction summary");
    }
    if set(line.queue_transcript_only) {
        return UserLine::Bookkeeping("transcript-only line");
    }
    if line.prompt_source.is_some() {
        return UserLine::Prompt;
    }
    if set(line.is_meta) {
        return UserLine::Bookkeeping("meta line");
    }
    let head = text.trim_start();
    if head.is_empty() {
        UserLine::Tentative
    } else if head.starts_with("[Request interrupted") {
        UserLine::Interrupt
    } else if head.starts_with("<local-command-") {
        UserLine::LocalOutput
    } else if head.starts_with("<command-name>") || head.starts_with("<command-message>") {
        UserLine::Tentative
    } else {
        UserLine::Prompt
    }
}

/// Whether a message's content holds an image, for a prompt that has no words.
fn holds_image(content: &serde_json::Value) -> bool {
    content.as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.get("type").and_then(|t| t.as_str()) == Some("image"))
    })
}

/// Start the turn a prompt line opens, closing the one before it.
fn open_turn(
    out: &mut Recovered,
    bus_id: BusId,
    open: &mut Option<OpenTurn>,
    opened: &mut HashSet<String>,
    entry: Entry,
) {
    close(out, bus_id, open.take(), ClosedBy::NextPrompt);
    if let Some(id) = &entry.vendor_turn_id {
        opened.insert(id.as_str().to_string());
    }
    out.entries.push(entry);
    *open = Some(OpenTurn {
        last_reply: None,
        stop: ModelStop::Unsaid,
    });
}

/// A turn that has started and not yet been closed by the next prompt.
struct OpenTurn {
    /// The most recent reply in this turn: when it came, what it said, and the
    /// stamp of the line that carried it. Only the last one survives, matching
    /// what `Stop` sends on the live path. The stamp is the reply's own — a
    /// vendor upgrade mid-conversation is ordinary, and the ending belongs to
    /// the build that wrote it, not to the one that read the prompt.
    last_reply: Option<(Option<Timestamp>, String, Source)>,
    /// What the turn's last line said about the model. A Chat export holds
    /// finished messages only, so its turns are always stopped.
    stop: ModelStop,
}

/// What closed a turn, which is what an unsaid stop is read against.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ClosedBy {
    /// The next prompt: the turn is over, however it went.
    NextPrompt,
    /// The end of the file: the turn may still be running.
    EndOfFile,
}

/// The text a message carries, ignoring everything that is not text.
///
/// A user message is either a bare string or a list of blocks; an assistant
/// message is always a list. Only `text` blocks are read — `tool_use`,
/// `tool_result` and `thinking` are deliberately not recovered, because they
/// are the overwhelming majority of a transcript's bytes and the default
/// retention level would elide them anyway.
fn text_of(content: &serde_json::Value) -> Option<String> {
    if let Some(text) = content.as_str() {
        return (!text.is_empty()).then(|| text.to_string());
    }
    let joined: Vec<&str> = content
        .as_array()?
        .iter()
        .filter(|block| block.get("type").and_then(|t| t.as_str()) == Some("text"))
        .filter_map(|block| block.get("text")?.as_str())
        .collect();
    (!joined.is_empty()).then(|| joined.join("\n"))
}

/// Recover the conversation from one transcript.
///
/// Turns are paired by file order: an assistant reply belongs to the most
/// recent user prompt, and the last reply of a turn is the one that ends it.
/// That is an assumption rather than a fact — assistant lines carry no
/// `promptId`, unlike user lines, so there is nothing to join on. It held in
/// every transcript sampled.
///
/// `session` is the id the file is named for, and only its own lines are read.
/// A transcript can open on another session's lines: a Claude Desktop
/// continuation starts with its parent's history, stamped with the parent's
/// `sessionId` (14 of 2,392 real files, 21,920 such `user` lines, always before
/// the file's own first line). Those turns belong to the parent's conversation
/// and are already in the parent's own file.
pub fn parse_claude_transcript(text: &str, session: &str, bus_id: BusId) -> Recovered {
    let mut out = Recovered {
        vendor_session_id: Some(session.to_string()),
        ..Recovered::default()
    };
    let mut open: Option<OpenTurn> = None;
    // A prompt that is a turn only if the model answers it.
    let mut tentative: Option<Entry> = None;
    // The vendor numbers its turns: every line of one carries the same
    // `promptId`, so a skill's body or a second block of one prompt starts
    // nothing.
    let mut opened: HashSet<String> = HashSet::new();
    let mut harness_version: Option<String> = None;

    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let Ok(line) = serde_json::from_str::<Line>(raw) else {
            out.skip("unreadable json");
            continue;
        };
        if line.is_sidechain == Some(true) {
            out.skip("sidechain");
            continue;
        }
        if line.session_id.as_deref().is_some_and(|id| id != session) {
            out.skip("line of another session");
            continue;
        }
        // Kept as the last one seen rather than the first: a transcript's
        // opening lines are bookkeeping that carries no `cwd`, and a session
        // that moved should be remembered where it ended up.
        if line.cwd.is_some() {
            out.workspace.clone_from(&line.cwd);
        }
        if let Some(entrypoint) = &line.entrypoint {
            out.entrypoint = Some(Entrypoint::new(entrypoint.clone()));
        }
        let at = line
            .timestamp
            .as_deref()
            .and_then(|text| text.parse::<Timestamp>().ok());
        let source = Source {
            harness: Harness::new(Harness::CLAUDE_CODE),
            // The transcript carries the vendor version on every message line,
            // which the live hook path never sees.
            harness_version: line.version.clone(),
            vendor_session_id: out.vendor_session_id.clone(),
            workspace: out.workspace.clone(),
            entrypoint: out.entrypoint.clone(),
            backfilled_from: Some(CLAUDE_TRANSCRIPT.to_string()),
            profile: None,
        };
        if harness_version.is_none() {
            harness_version.clone_from(&line.version);
        }
        let kind = line.r#type.as_deref().unwrap_or("(none)");

        match kind {
            "user" => {
                let content = line.message.as_ref().and_then(|m| m.content.as_ref());
                let Some(text) = content.and_then(text_of).or_else(|| {
                    // No words, and still somebody's prompt: the hook sees it
                    // with an empty one.
                    content.filter(|c| holds_image(c)).map(|_| String::new())
                }) else {
                    out.skip("user line with no text");
                    continue;
                };
                let kind = read_user_line(&line, &text);
                match kind {
                    UserLine::Bookkeeping(why) => {
                        out.skip(why);
                        continue;
                    }
                    // The vendor fires no `Stop` for an interrupt, so the
                    // recorder writes no ending for it, and neither does this.
                    UserLine::Interrupt => {
                        open = None;
                        tentative = None;
                        out.skip("interrupt line");
                        continue;
                    }
                    UserLine::LocalOutput => {
                        tentative = None;
                        out.skip("local command output");
                        continue;
                    }
                    UserLine::Prompt | UserLine::Tentative => {}
                }
                if line
                    .prompt_id
                    .as_ref()
                    .is_some_and(|id| opened.contains(id))
                {
                    out.skip("another line of an open turn");
                    continue;
                }
                let entry = Entry {
                    bus_id,
                    observed_at: at,
                    source: source.clone(),
                    vendor_turn_id: line.prompt_id.clone().map(VendorTurnId::new),
                    vendor_agent_id: None,
                    hook_id: None,
                    spooled_at: None,
                    event: Event::TurnStarted {
                        author: line.prompt_source.clone().map(Author::new),
                        prompt: Body::kept(text),
                    },
                };
                if kind == UserLine::Tentative {
                    tentative = Some(entry);
                    continue;
                }
                tentative = None;
                open_turn(&mut out, bus_id, &mut open, &mut opened, entry);
            }
            "assistant" => {
                let message = line.message.as_ref();
                if message.and_then(|m| m.model.as_deref()) == Some(VENDOR_WROTE_THIS) {
                    out.skip("assistant line the vendor wrote");
                    continue;
                }
                if let Some(entry) = tentative.take() {
                    open_turn(&mut out, bus_id, &mut open, &mut opened, entry);
                }
                if let Some(turn) = open.as_mut() {
                    turn.stop = ModelStop::of(message.and_then(|m| m.stop_reason.as_deref()));
                }
                let Some(text) = message.and_then(|m| m.content.as_ref()).and_then(text_of) else {
                    out.skip("assistant line with no text");
                    continue;
                };
                match open.as_mut() {
                    Some(turn) => turn.last_reply = Some((at, text, source)),
                    None => out.skip("assistant line before any prompt"),
                }
            }
            other => out.skip(other),
        }
    }
    close(&mut out, bus_id, open.take(), ClosedBy::EndOfFile);
    prepend_session_candidate(&mut out, bus_id, harness_version);
    out
}

/// Put a session record at the head of a recovered conversation.
///
/// Backfill used to emit turns and nothing else, so a recovered conversation
/// had no record of the session it happened in and therefore no workspace. That
/// is not a cosmetic gap: the workspace is the key a continuation is looked up
/// by, and it left 3,425 of 3,433 conversations invisible to that lookup.
///
/// It carries no `trigger`, because the transcript does not say how the session
/// began and inventing `startup` would be a guess in a record whose whole
/// purpose is to be a weaker, clearly-labelled fact. It is stamped with the
/// first line's time, not with now, so a reader sorting by `ts` still sees the
/// session open before its first turn.
fn prepend_session_candidate(out: &mut Recovered, bus_id: BusId, harness_version: Option<String>) {
    if out.entries.is_empty() {
        return;
    }
    let observed_at = out.entries[0].observed_at;
    out.entries.insert(
        0,
        Entry {
            bus_id,
            observed_at,
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version,
                vendor_session_id: out.vendor_session_id.clone(),
                workspace: out.workspace.clone(),
                entrypoint: out.entrypoint.clone(),
                backfilled_from: Some(CLAUDE_TRANSCRIPT.to_string()),
                profile: None,
            },
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event: Event::SessionCandidate { trigger: None },
        },
    );
}

/// Names the source a Codex thread was read from: the app-server's
/// `thread/list` and `thread/turns/list`, not its rollout files.
pub const CODEX_THREAD: &str = "codex_thread";

/// Recover one Codex thread from what the app-server said about it.
///
/// Probed on 2026-09-03 against Codex 0.152.1: a `thread/list` item carries
/// `id` — equal to `sessionId`, equal to the hook's `session_id`, so it can
/// *be* the `bus_id` the way a Claude transcript's file name can — plus
/// `cwd`, `source` (`cli`, `vscode`), `cliVersion` and `createdAt` (unix
/// seconds, as a string). A `thread/turns/list` turn carries `id`, `status`,
/// `startedAt`/`completedAt` (numbers) and items of type `userMessage` (text
/// blocks) and `agentMessage` (`text`, `phase`). The app-server read threads
/// back to 0.107.0 on the same machine, so the version skew the plan feared
/// belongs to rollout files, not to this surface. Read tolerantly, like the
/// transcript parser: what it does not understand is counted, not fatal.
pub fn parse_codex_thread(thread: &serde_json::Value, turns: &[serde_json::Value]) -> Recovered {
    let mut out = Recovered::default();
    let Some(bus_id) = thread
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| id.parse::<BusId>().ok())
    else {
        out.skip("thread id is not a uuid");
        return out;
    };
    let text = |key: &str| {
        thread
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    out.vendor_session_id = text("sessionId").or_else(|| Some(bus_id.to_string()));
    out.workspace = text("cwd");
    out.entrypoint = text("source").map(Entrypoint::new);
    let source = Source {
        harness: Harness::new(Harness::CODEX),
        harness_version: text("cliVersion"),
        vendor_session_id: out.vendor_session_id.clone(),
        workspace: out.workspace.clone(),
        entrypoint: out.entrypoint.clone(),
        backfilled_from: Some(CODEX_THREAD.to_string()),
        profile: None,
    };

    // A reply belongs to the most recent prompt, and the last reply is the
    // one that ends it — the transcript parser's rule, needed here because
    // the VS Code extension's legacy history splits one exchange into a
    // prompt turn and reply-only turns (590 of 1,070 turns in the 60 most
    // recent threads on 2026-09-03 carried nothing but replies).
    let mut open: Option<OpenCodexTurn> = None;
    let thread_minted = thread
        .get("id")
        .and_then(serde_json::Value::as_str)
        .and_then(minted_ms);
    for turn in turns {
        // A fork lists its parent's turns as its own, and they are the parent's.
        let turn_minted = turn
            .get("id")
            .and_then(serde_json::Value::as_str)
            .and_then(minted_ms);
        if thread_minted
            .zip(turn_minted)
            .is_some_and(|(thread, turn)| turn < thread)
        {
            out.skip("turn older than its thread");
            continue;
        }
        let items: Vec<&serde_json::Value> = turn
            .get("items")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .collect();
        let mut prompt: Vec<&str> = Vec::new();
        let mut saw_user = false;
        let mut answer: Option<&str> = None;
        for item in &items {
            match item.get("type").and_then(serde_json::Value::as_str) {
                Some("userMessage") => {
                    saw_user = true;
                    prompt.extend(
                        item.get("content")
                            .and_then(serde_json::Value::as_array)
                            .into_iter()
                            .flatten()
                            .filter(|block| {
                                block.get("type").and_then(|t| t.as_str()) == Some("text")
                            })
                            .filter_map(|block| block.get("text")?.as_str()),
                    );
                }
                Some("agentMessage") => {
                    if let Some(said) = item.get("text").and_then(serde_json::Value::as_str) {
                        answer = Some(said);
                    }
                }
                Some(other) => out.skip(format!("item type {other}")),
                None => out.skip("item without a type"),
            }
        }
        let completed = turn.get("status").and_then(serde_json::Value::as_str) == Some("completed");
        let completed_at = unix_seconds(turn.get("completedAt"));
        if prompt.is_empty() {
            if saw_user {
                // An image-only prompt, most often. Its text is not on this
                // surface; inventing one would be a fabrication.
                out.skip("user message without text");
            }
            match open.as_mut() {
                Some(current) => {
                    if let Some(said) = answer {
                        current.reply = Some((completed_at, said.to_string()));
                    }
                    current.completed = completed;
                }
                None => out.skip("reply before any prompt"),
            }
            continue;
        }
        close_codex(&mut out, bus_id, &source, open.take());
        let vendor_turn_id = turn
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(VendorTurnId::new);
        out.entries.push(Entry {
            bus_id,
            observed_at: unix_seconds(turn.get("startedAt")),
            source: source.clone(),
            vendor_turn_id: vendor_turn_id.clone(),
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event: Event::TurnStarted {
                author: None,
                prompt: Body::kept(prompt.join("\n")),
            },
        });
        open = Some(OpenCodexTurn {
            vendor_turn_id,
            reply: answer.map(|said| (completed_at, said.to_string())),
            completed,
        });
    }
    close_codex(&mut out, bus_id, &source, open.take());

    if out.entries.is_empty() {
        return out;
    }
    let observed_at = unix_seconds(thread.get("createdAt")).or(out.entries[0].observed_at);
    out.entries.insert(
        0,
        Entry {
            bus_id,
            observed_at,
            source,
            vendor_turn_id: None,
            vendor_agent_id: None,
            hook_id: None,
            spooled_at: None,
            event: Event::SessionCandidate { trigger: None },
        },
    );
    out
}

/// When a UUIDv7 says it was minted, in milliseconds. Any other id says nothing.
fn minted_ms(id: &str) -> Option<u64> {
    let id = uuid::Uuid::try_parse(id).ok()?;
    (id.get_version_num() == 7).then(|| (id.as_u128() >> 80) as u64)
}

/// A Codex prompt that has not yet been ended by the next prompt.
struct OpenCodexTurn {
    vendor_turn_id: Option<VendorTurnId>,
    /// The most recent reply: when it completed and what it said.
    reply: Option<(Option<Timestamp>, String)>,
    /// Whether the last turn that touched it completed. A turn still in
    /// progress, or one that never completed, has no ending — which is the
    /// truth about it.
    completed: bool,
}

/// End a Codex prompt with the last reply it received.
fn close_codex(out: &mut Recovered, bus_id: BusId, source: &Source, open: Option<OpenCodexTurn>) {
    let Some(OpenCodexTurn {
        vendor_turn_id,
        reply,
        completed: true,
    }) = open
    else {
        return;
    };
    let (observed_at, said) = match reply {
        Some((at, said)) => (at, Some(Body::kept(said))),
        None => (None, None),
    };
    out.entries.push(Entry {
        bus_id,
        observed_at,
        source: source.clone(),
        vendor_turn_id,
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
        event: Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: said,
            tokens: None,
        },
    });
}

/// Unix seconds as the app-server writes them: a number on a turn, a string
/// on a thread.
fn unix_seconds(value: Option<&serde_json::Value>) -> Option<Timestamp> {
    let seconds = match value {
        Some(serde_json::Value::Number(n)) => n.as_i64(),
        Some(serde_json::Value::String(s)) => s.parse().ok(),
        _ => None,
    }?;
    Timestamp::from_unix_seconds(seconds)
}

/// End a turn with the last reply it received, if the turn is known to be over.
///
/// A prompt that was never answered simply has no ending in the transcript,
/// which is the truth about it. Neither has a turn the model had not stopped when
/// the source ran out or the next prompt came: its last text is partial, and
/// `Complete` would say otherwise for good. A turn still running gets its ending
/// from `Stop`.
fn close(out: &mut Recovered, bus_id: BusId, open: Option<OpenTurn>, closed_by: ClosedBy) {
    let Some(OpenTurn {
        last_reply: Some((at, text, source)),
        stop,
    }) = open
    else {
        return;
    };
    let over = match stop {
        ModelStop::Stopped => true,
        ModelStop::Working => false,
        ModelStop::Unsaid => closed_by == ClosedBy::NextPrompt,
    };
    if !over {
        out.skip("reply of a turn not known to have ended");
        return;
    }
    out.entries.push(Entry {
        bus_id,
        observed_at: at,
        source,
        vendor_turn_id: None,
        vendor_agent_id: None,
        hook_id: None,
        spooled_at: None,
        event: Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: Some(Body::kept(text)),
            tokens: None,
        },
    });
}

/// Official Claude.ai / Desktop data-export archive (`conversations.json`).
///
/// Desktop and `claude import-conversations` both read this shape. The
/// importer hashes `uuid` into a new Code session id; we keep the Chat
/// uuid so the window's conversation stays itself.
pub const CLAUDE_CHAT_EXPORT: &str = "claude_chat_export";

/// One official export file. Unknown schemas are counted, not guessed.
///
/// Privacy → Export data writes a top-level array. Desktop / some wrappers
/// wrap the same objects as `{ "conversations": [...] }`. Both keep `uuid`.
pub fn parse_claude_chat_export(text: &str) -> Vec<Recovered> {
    let Ok(root) = serde_json::from_str::<Value>(text) else {
        let mut out = Recovered::default();
        out.skip("unreadable json");
        return vec![out];
    };
    let Some(conversations) = chat_export_conversations(&root) else {
        let mut out = Recovered::default();
        out.skip("unknown schema");
        return vec![out];
    };
    conversations.iter().map(parse_chat_conversation).collect()
}

fn chat_export_conversations(root: &Value) -> Option<&Vec<Value>> {
    match root {
        Value::Array(conversations) => Some(conversations),
        Value::Object(_) => root.get("conversations").and_then(Value::as_array),
        _ => None,
    }
}

/// Official export puts the body on `text`, and also on `content[].text`.
fn chat_message_text(message: &Value) -> Option<&str> {
    message
        .get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .or_else(|| {
            message
                .get("content")
                .and_then(Value::as_array)
                .and_then(|blocks| {
                    blocks.iter().find_map(|block| {
                        (block.get("type").and_then(Value::as_str) == Some("text"))
                            .then(|| block.get("text").and_then(Value::as_str))
                            .flatten()
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                    })
                })
        })
}

fn parse_chat_conversation(conversation: &Value) -> Recovered {
    let mut out = Recovered::default();
    let Some(uuid) = conversation.get("uuid").and_then(Value::as_str) else {
        out.skip("conversation has no uuid");
        return out;
    };
    let Some(bus_id) = uuid.parse::<BusId>().ok() else {
        out.skip("conversation uuid is not a uuid");
        return out;
    };
    out.vendor_session_id = Some(uuid.to_string());
    let vendor_session_id = out.vendor_session_id.clone();
    let source = || Source {
        harness: Harness::new(Harness::CLAUDE_CHAT),
        harness_version: None,
        vendor_session_id: vendor_session_id.clone(),
        workspace: None,
        entrypoint: Some(Entrypoint::new("claude_ai_chat")),
        backfilled_from: Some(CLAUDE_CHAT_EXPORT.to_string()),
        profile: None,
    };
    let Some(messages) = conversation.get("chat_messages").and_then(Value::as_array) else {
        out.skip("conversation has no chat_messages");
        return out;
    };
    let mut open: Option<OpenTurn> = None;
    for message in messages {
        let sender = message.get("sender").and_then(Value::as_str).unwrap_or("");
        let text = chat_message_text(message);
        let at = message
            .get("created_at")
            .and_then(Value::as_str)
            .and_then(|text| text.parse::<Timestamp>().ok());
        match sender {
            "human" => {
                let Some(text) = text else {
                    out.skip("human message with no text");
                    continue;
                };
                close(&mut out, bus_id, open.take(), ClosedBy::NextPrompt);
                out.entries.push(Entry {
                    bus_id,
                    observed_at: at,
                    source: source(),
                    vendor_turn_id: message
                        .get("uuid")
                        .and_then(Value::as_str)
                        .map(VendorTurnId::new),
                    vendor_agent_id: None,
                    hook_id: None,
                    spooled_at: None,
                    event: Event::TurnStarted {
                        author: Some(Author::new("human")),
                        prompt: Body::kept(text.to_string()),
                    },
                });
                open = Some(OpenTurn {
                    last_reply: None,
                    stop: ModelStop::Stopped,
                });
            }
            "assistant" => {
                let Some(text) = text else {
                    out.skip("assistant message with no text");
                    continue;
                };
                match open.as_mut() {
                    Some(turn) => turn.last_reply = Some((at, text.to_string(), source())),
                    None => out.skip("assistant line before any prompt"),
                }
            }
            other => out.skip(if other.is_empty() {
                "message with no sender"
            } else {
                other
            }),
        }
    }
    close(&mut out, bus_id, open.take(), ClosedBy::EndOfFile);
    if !out.entries.is_empty() {
        let observed_at = out.entries[0].observed_at;
        out.entries.insert(
            0,
            Entry {
                bus_id,
                observed_at,
                source: source(),
                vendor_turn_id: None,
                vendor_agent_id: None,
                hook_id: None,
                spooled_at: None,
                event: Event::SessionCandidate { trigger: None },
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped from real transcripts: a user line carries `promptId`,
    /// `promptSource`, `version` and a bare-string content; an assistant line
    /// carries a block list and no `promptId` at all.
    const TRANSCRIPT: &str = r#"
{"type":"queue-operation","sessionId":"S1","timestamp":"2026-07-28T15:36:40.000Z"}
{"type":"user","sessionId":"S1","timestamp":"2026-07-28T15:36:42.867Z","version":"2.1.229","cwd":"/Applications/conbus","promptId":"P1","message":{"role":"user","content":"first question"}}
{"type":"attachment","sessionId":"S1","subtype":"hook_success"}
{"type":"assistant","sessionId":"S1","timestamp":"2026-07-28T15:36:50.000Z","version":"2.1.229","message":{"role":"assistant","content":[{"type":"text","text":"thinking about it"}]}}
{"type":"assistant","sessionId":"S1","timestamp":"2026-07-28T15:36:56.388Z","version":"2.1.229","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}
{"type":"assistant","sessionId":"S1","timestamp":"2026-07-28T15:37:00.000Z","version":"2.1.229","message":{"role":"assistant","content":[{"type":"text","text":"the answer"}]}}
{"type":"user","sessionId":"S1","timestamp":"2026-07-28T15:38:00.000Z","version":"2.1.229","promptId":"P2","promptSource":"sdk","message":{"role":"user","content":[{"type":"text","text":"second question"}]}}
{"type":"assistant","sessionId":"S1","timestamp":"2026-07-28T15:38:10.000Z","version":"2.1.247","message":{"role":"assistant","stop_reason":"end_turn","content":[{"type":"text","text":"and the second answer"}]}}
"#;

    fn parsed() -> Recovered {
        parse_claude_transcript(TRANSCRIPT, "S1", BusId::mint())
    }

    /// The turn records, without the session record that now heads a recovered
    /// conversation. The tests below are about turns and should not move every
    /// time something else joins the head of the list.
    fn turns(out: &Recovered) -> Vec<&Entry> {
        out.entries
            .iter()
            .filter(|entry| !matches!(entry.event, Event::SessionCandidate { .. }))
            .collect()
    }

    #[test]
    fn a_transcript_yields_its_turns_and_nothing_else() {
        let out = parsed();

        let kinds: Vec<&str> = out
            .entries
            .iter()
            .map(|e| match &e.event {
                Event::SessionCandidate { .. } => "session",
                Event::TurnStarted { .. } => "started",
                Event::TurnEnded { .. } => "ended",
                other => panic!("backfill produced {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["session", "started", "ended", "started", "ended"]
        );
        assert_eq!(out.vendor_session_id.as_deref(), Some("S1"));
    }

    #[test]
    fn a_turn_ends_once_with_its_last_reply() {
        // Three assistant lines answer the first prompt: text, a tool call, and
        // the closing text. One turn, one ending, and the ending is the last
        // text — the same thing `Stop.last_assistant_message` carries live.
        let out = parsed();

        let turns = turns(&out);
        let Event::TurnEnded {
            assistant_message: Some(message),
            ..
        } = &turns[1].event
        else {
            panic!("expected an ended turn, got {:?}", turns[1].event);
        };
        assert_eq!(*message, Body::kept("the answer"));
    }

    #[test]
    fn the_author_the_live_path_never_sees_is_recovered_here() {
        // `UserPromptSubmit.source` is compiled out of the hook payload, so the
        // transcript's `promptSource` is the only place a turn's author is
        // recorded at all. Absent stays absent — never read as human.
        let out = parsed();

        let turns = turns(&out);
        let Event::TurnStarted { author, .. } = &turns[0].event else {
            panic!("expected a started turn");
        };
        assert_eq!(*author, None);
        let Event::TurnStarted { author, .. } = &turns[2].event else {
            panic!("expected a started turn");
        };
        assert_eq!(author.as_ref().map(Author::as_str), Some("sdk"));
    }

    #[test]
    fn every_recovered_record_says_it_was_recovered() {
        let out = parsed();

        assert!(
            out.entries
                .iter()
                .all(|e| e.source.backfilled_from.as_deref() == Some(CLAUDE_TRANSCRIPT)),
            "a backfilled record that looks live will be over-trusted"
        );
    }

    #[test]
    fn records_carry_the_time_they_happened_not_the_time_they_were_read() {
        let out = parsed();

        assert_eq!(
            out.entries[0].observed_at.map(|t| t.to_string()).as_deref(),
            Some("2026-07-28T15:36:42.867Z")
        );
    }

    #[test]
    fn the_workspace_is_recovered_so_backfilled_conversations_can_be_found_again() {
        // The workspace is how a second harness finds the conversation it is
        // continuing, and backfill is where nearly all the conversations are:
        // 3,425 of 3,433 in a real log. Recovering everything else and leaving
        // them workspace-less makes them invisible to that lookup.
        //
        // The first lines of a transcript are bookkeeping and carry no `cwd`,
        // so a parser that only read line one would recover nothing.
        let out = parsed();

        assert_eq!(out.workspace.as_deref(), Some("/Applications/conbus"));
        assert!(
            out.entries
                .iter()
                .all(|entry| entry.source.workspace.as_deref() == Some("/Applications/conbus")),
            "a recovered record without its workspace cannot be found again"
        );
    }

    #[test]
    fn a_recovered_conversation_says_where_it_happened() {
        // Backfill used to emit turns and nothing else, so a recovered
        // conversation had no session record and therefore no workspace — which
        // is the key a continuation is looked up by. It left 3,425 of 3,433
        // conversations in a real log invisible to that lookup.
        let out = parsed();

        let head = &out.entries[0];
        let Event::SessionCandidate { trigger } = &head.event else {
            panic!("a recovered conversation must open with its session");
        };
        assert_eq!(
            *trigger, None,
            "the transcript does not say how the session began, and guessing \
             `startup` would put a fabrication in the weakest record we write"
        );
        assert_eq!(
            head.source.workspace.as_deref(),
            Some("/Applications/conbus")
        );
        assert_eq!(
            head.observed_at, out.entries[1].observed_at,
            "the session must be stamped with its own conversation's time, so a \
             reader sorting by ts still sees it open before the first turn"
        );
        assert!(
            out.entries
                .iter()
                .all(|entry| entry.source.workspace.as_deref() == Some("/Applications/conbus")),
        );
    }

    #[test]
    fn a_conversation_that_recovered_nothing_gets_no_session_record_either() {
        // An empty recovery is not a conversation. Emitting a bare session for
        // it would put ghosts in the log, which is the thing the live path's
        // whole deferral design exists to avoid.
        let out = parse_claude_transcript(
            r#"{"type":"queue-operation","sessionId":"S9","cwd":"/tmp"}"#,
            "S9",
            BusId::mint(),
        );

        assert!(out.entries.is_empty());
    }

    #[test]
    fn the_vendor_version_the_hook_never_sends_is_recovered_here() {
        let out = parsed();

        let turns = turns(&out);
        assert_eq!(turns[0].source.harness_version.as_deref(), Some("2.1.229"));
        assert_eq!(turns[3].source.harness_version.as_deref(), Some("2.1.247"));
    }

    #[test]
    fn the_prompt_correlator_is_recovered_and_replies_carry_none() {
        let out = parsed();

        let turns = turns(&out);
        assert_eq!(
            turns[0].vendor_turn_id.as_ref().map(VendorTurnId::as_str),
            Some("P1")
        );
        assert_eq!(
            turns[1].vendor_turn_id, None,
            "an assistant line has no promptId, so inventing one would be a fabrication"
        );
    }

    #[test]
    fn what_it_did_not_read_is_counted_rather_than_dropped_in_silence() {
        let out = parsed();

        assert_eq!(out.skipped.get("queue-operation"), Some(&1));
        assert_eq!(out.skipped.get("attachment"), Some(&1));
        assert_eq!(
            out.skipped.get("assistant line with no text"),
            Some(&1),
            "the tool_use-only reply must be counted, not ignored"
        );
    }

    #[test]
    fn a_line_shape_this_parser_cannot_read_costs_that_line_only() {
        // The whole reason backfill is allowed to touch a format the vendor
        // calls internal: a failure here thins the corpus, it does not lose a
        // conversation.
        let damaged = TRANSCRIPT.replace(
            r#"{"type":"attachment","sessionId":"S1","subtype":"hook_success"}"#,
            "{not json at all",
        );

        let out = parse_claude_transcript(&damaged, "S1", BusId::mint());

        assert_eq!(turns(&out).len(), 4, "the conversation must survive");
        assert_eq!(out.skipped.get("unreadable json"), Some(&1));
    }

    #[test]
    fn subagent_traffic_is_left_out_of_the_main_conversation() {
        let with_sidechain = format!(
            "{TRANSCRIPT}\n{}",
            r#"{"type":"user","sessionId":"S1","isSidechain":true,"timestamp":"2026-07-28T15:39:00.000Z","promptId":"P9","message":{"role":"user","content":"a subagent prompt"}}"#
        );

        let out = parse_claude_transcript(&with_sidechain, "S1", BusId::mint());

        assert_eq!(turns(&out).len(), 4);
        assert_eq!(out.skipped.get("sidechain"), Some(&1));
    }

    /// A continuation: the parent's lines first, under the parent's id.
    const FORK: &str = r#"
{"type":"user","sessionId":"PARENT","timestamp":"2026-10-01T00:00:00.000Z","version":"2.1.200","cwd":"/parent/checkout","entrypoint":"claude-desktop","promptId":"P0","message":{"role":"user","content":"the parent's prompt"}}
{"type":"assistant","sessionId":"PARENT","timestamp":"2026-10-01T00:00:01.000Z","version":"2.1.200","message":{"role":"assistant","content":[{"type":"text","text":"the parent's reply"}]}}
{"type":"user","sessionId":"CHILD","timestamp":"2026-11-01T00:00:00.000Z","version":"2.1.316","cwd":"/child/checkout","entrypoint":"cli","promptId":"C0","message":{"role":"user","content":"the fork's prompt"}}
"#;

    #[test]
    fn lines_of_another_session_do_not_become_this_sessions_turns() {
        let out = parse_claude_transcript(FORK, "CHILD", BusId::mint());

        assert_eq!(out.vendor_session_id.as_deref(), Some("CHILD"));
        let started: Vec<&Entry> = turns(&out)
            .into_iter()
            .filter(|e| matches!(e.event, Event::TurnStarted { .. }))
            .collect();
        assert_eq!(started.len(), 1);
        assert_eq!(
            started[0].vendor_turn_id.as_ref().map(VendorTurnId::as_str),
            Some("C0")
        );
        assert_eq!(out.skipped.get("line of another session"), Some(&2));
    }

    #[test]
    fn a_forks_records_name_the_fork_and_nothing_of_the_parent() {
        // A parent's stamp under the fork would bind the parent's session to
        // the fork on the next restore, and its old time would move the fork's
        // last activity backwards.
        let out = parse_claude_transcript(FORK, "CHILD", BusId::mint());

        assert!(out.entries.iter().all(|entry| {
            entry.source.vendor_session_id.as_deref() == Some("CHILD")
                && entry.source.workspace.as_deref() == Some("/child/checkout")
                && entry.source.harness_version.as_deref() == Some("2.1.316")
                && entry.observed_at.map(|t| t.to_string()).as_deref()
                    == Some("2026-11-01T00:00:00.000Z")
        }));
        assert_eq!(out.entrypoint.as_ref().map(Entrypoint::as_str), Some("cli"));
    }

    #[test]
    fn a_file_holding_only_another_sessions_lines_recovers_nothing() {
        let out = parse_claude_transcript(FORK, "SOMEONE-ELSE", BusId::mint());

        assert!(out.entries.is_empty());
        assert_eq!(out.skipped.get("line of another session"), Some(&3));
    }

    /// The prompts a parse started, as their text.
    fn prompts(out: &Recovered) -> Vec<String> {
        out.entries
            .iter()
            .filter_map(|entry| match &entry.event {
                Event::TurnStarted { prompt, .. } => match prompt {
                    Body::Kept { text } => Some(text.clone()),
                    other => panic!("{other:?}"),
                },
                _ => None,
            })
            .collect()
    }

    /// The endings a parse wrote, as their text.
    fn endings(out: &Recovered) -> Vec<String> {
        out.entries
            .iter()
            .filter_map(|entry| match &entry.event {
                Event::TurnEnded {
                    assistant_message: Some(Body::Kept { text }),
                    ..
                } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }

    /// A `user` line: `extra` is the flags, in the shapes real transcripts use.
    fn user(extra: &str, id: &str, text: &str) -> String {
        format!(
            r#"{{"type":"user","sessionId":"S1","timestamp":"2026-11-01T00:00:00.000Z","promptId":"{id}",{extra}"message":{{"role":"user","content":{}}}}}"#,
            serde_json::json!(text)
        )
    }

    fn assistant(text: &str) -> String {
        format!(
            r#"{{"type":"assistant","sessionId":"S1","timestamp":"2026-11-01T00:00:01.000Z","message":{{"role":"assistant","model":"m","stop_reason":"end_turn","content":[{{"type":"text","text":{}}}]}}}}"#,
            serde_json::json!(text)
        )
    }

    fn parse(lines: &[String]) -> Recovered {
        parse_claude_transcript(&lines.join("\n"), "S1", BusId::mint())
    }

    #[test]
    fn lines_the_vendor_marks_as_not_a_prompt_do_not_become_turns() {
        let out = parse(&[
            user("", "P1", "a real prompt"),
            user(
                r#""isMeta":true,"#,
                "P2",
                "Base directory for this skill: /x",
            ),
            user(
                r#""isCompactSummary":true,"isVisibleInTranscriptOnly":true,"#,
                "P3",
                "This session is being continued from a previous conversation",
            ),
            user("", "P4", "[Request interrupted by user]"),
            user(
                r#""promptSource":"system","queueTranscriptOnly":true,"#,
                "P5",
                "<task-notification>not sent to the model</task-notification>",
            ),
        ]);

        assert_eq!(prompts(&out), vec!["a real prompt"]);
    }

    #[test]
    fn a_skill_body_does_not_end_the_turn_it_belongs_to() {
        let out = parse(&[
            user(r#""promptSource":"sdk","#, "P1", "use the skill"),
            user(
                r#""isMeta":true,"#,
                "P1",
                "Base directory for this skill: /x",
            ),
            assistant("the answer"),
        ]);

        assert_eq!(prompts(&out), vec!["use the skill"]);
        assert_eq!(endings(&out), vec!["the answer"]);
    }

    #[test]
    fn a_compaction_inside_a_turn_does_not_end_it() {
        let out = parse(&[
            user(r#""promptSource":"sdk","#, "P1", "a long job"),
            user(
                r#""isCompactSummary":true,"isVisibleInTranscriptOnly":true,"#,
                "P1",
                "This session is being continued from a previous conversation",
            ),
            assistant("done after the compaction"),
        ]);

        assert_eq!(prompts(&out), vec!["a long job"]);
        assert_eq!(endings(&out), vec!["done after the compaction"]);
    }

    #[test]
    fn a_local_command_is_not_a_turn() {
        // `/model` and friends: the caveat, the command, its output. The model
        // never hears of it.
        let out = parse(&[
            user(
                r#""isMeta":true,"#,
                "P1",
                "<local-command-caveat>Caveat</local-command-caveat>",
            ),
            user("", "P1", "<command-name>/model</command-name>"),
            user(
                "",
                "P1",
                "<local-command-stdout>Set model</local-command-stdout>",
            ),
        ]);

        assert!(out.entries.is_empty(), "{:?}", out.entries);
    }

    #[test]
    fn a_local_command_with_no_output_is_not_a_turn_either() {
        let out = parse(&[user("", "P1", "<command-name>/exit</command-name>")]);

        assert!(out.entries.is_empty(), "{:?}", out.entries);
    }

    #[test]
    fn a_prompt_behind_a_local_command_on_the_same_id_is_the_turn() {
        let out = parse(&[
            user(
                r#""isMeta":true,"#,
                "P1",
                "<local-command-caveat>Caveat</local-command-caveat>",
            ),
            user("", "P1", "<command-name>/model</command-name>"),
            user(
                "",
                "P1",
                "<local-command-stdout>Set model</local-command-stdout>",
            ),
            user(r#""promptSource":"sdk","#, "P1", "the real prompt"),
            assistant("the answer"),
        ]);

        assert_eq!(prompts(&out), vec!["the real prompt"]);
        assert_eq!(endings(&out), vec!["the answer"]);
    }

    #[test]
    fn a_slash_command_the_model_answers_is_a_turn() {
        // Typed by a person, and the hook fires for it. It carries no
        // `promptSource`, and only the model answering tells it from `/model`.
        let out = parse(&[
            user(
                "",
                "P1",
                "<command-message>go</command-message>\n<command-name>/go</command-name>",
            ),
            user(r#""isMeta":true,"#, "P1", "the command's expanded body"),
            assistant("went"),
        ]);

        assert_eq!(out.entries.len(), 3, "session, prompt, ending");
        assert_eq!(
            prompts(&out),
            vec!["<command-message>go</command-message>\n<command-name>/go</command-name>"]
        );
        assert_eq!(endings(&out), vec!["went"]);
    }

    #[test]
    fn a_turn_before_a_slash_command_keeps_its_own_ending() {
        let out = parse(&[
            user("", "P1", "first"),
            assistant("first answer"),
            user("", "P2", "<command-name>/go</command-name>"),
            assistant("went"),
        ]);

        assert_eq!(prompts(&out).len(), 2);
        assert_eq!(endings(&out), vec!["first answer", "went"]);
    }

    #[test]
    fn a_prompt_of_nothing_but_an_image_is_a_turn_with_no_words() {
        // The hook sees it with an empty prompt. The `[Image…]` marker after it
        // is the vendor's note about the image, marked meta.
        let image = r#"{"type":"user","sessionId":"S1","timestamp":"2026-11-01T00:00:00.000Z","promptId":"P1","promptSource":"sdk","message":{"role":"user","content":[{"type":"image","source":{"type":"base64","data":"AA=="}}]}}"#;
        let out = parse(&[
            image.to_string(),
            user(r#""isMeta":true,"#, "P1", "[Image: source: /tmp/a.png]"),
            assistant("a cat"),
        ]);

        assert_eq!(prompts(&out), vec![""]);
        assert_eq!(endings(&out), vec!["a cat"]);
    }

    #[test]
    fn a_meta_line_the_vendor_submitted_as_a_prompt_is_a_turn() {
        // Measured against the live log: `isMeta` with a `promptSource` is a
        // prompt the system or a peer sent. 522 of these fired the prompt hook,
        // and 5 meta lines without one did not.
        let out = parse(&[
            user(
                r#""isMeta":true,"promptSource":"system","#,
                "P1",
                "scheduled wake-up",
            ),
            assistant("woke"),
        ]);

        assert_eq!(prompts(&out), vec!["scheduled wake-up"]);
        assert_eq!(endings(&out), vec!["woke"]);
    }

    #[test]
    fn a_prompt_that_opens_on_a_system_reminder_is_still_a_prompt() {
        // SDK front ends put context in front of what the person typed. The
        // hook records the same text, so the tag says nothing about who wrote
        // the rest.
        let text = "<system-reminder>context</system-reminder>\nthe person's words";
        let out = parse(&[
            user(r#""promptSource":"sdk","#, "P1", text),
            assistant("ok"),
        ]);

        assert_eq!(prompts(&out), vec![text]);
    }

    #[test]
    fn words_a_person_typed_are_a_prompt_whatever_they_look_like() {
        let out = parse(&[user(
            r#""promptSource":"typed","#,
            "P1",
            "[Request interrupted by user] is what it printed, why?",
        )]);

        assert_eq!(out.entries.len(), 2, "{:?}", out.entries);
    }

    #[test]
    fn a_turn_the_person_interrupted_has_no_ending_as_on_the_live_path() {
        // The vendor fires no `Stop` for an interrupt, so the recorder writes
        // no ending, and neither may a recovery.
        let out = parse(&[
            user("", "P1", "start something"),
            assistant("working on it"),
            user("", "P1", "[Request interrupted by user]"),
            user("", "P2", "do this instead"),
            assistant("done"),
        ]);

        assert_eq!(prompts(&out), vec!["start something", "do this instead"]);
        assert_eq!(endings(&out), vec!["done"]);
    }

    #[test]
    fn one_prompt_id_opens_one_turn() {
        let out = parse(&[
            user(r#""promptSource":"sdk","#, "P1", "the prompt"),
            assistant("part one"),
            user(r#""promptSource":"sdk","#, "P1", "more of the same turn"),
            assistant("part two"),
        ]);

        assert_eq!(prompts(&out), vec!["the prompt"]);
        assert_eq!(endings(&out), vec!["part two"]);
    }

    #[test]
    fn the_vendors_own_message_is_not_the_models_reply() {
        // An API error or a usage limit is written as an assistant line by the
        // vendor itself. Live, `StopFailure` reports it; `Complete` would say
        // the opposite.
        let synthetic = r#"{"type":"assistant","sessionId":"S1","timestamp":"2026-11-01T00:00:01.000Z","isApiErrorMessage":true,"message":{"role":"assistant","model":"<synthetic>","stop_reason":"stop_sequence","content":[{"type":"text","text":"API Error: overloaded"}]}}"#;
        let out = parse(&[user("", "P1", "a prompt"), synthetic.to_string()]);

        assert_eq!(prompts(&out), vec!["a prompt"]);
        assert_eq!(endings(&out), Vec::<String>::new());
    }

    /// An assistant line that says why the model stopped, if it does.
    fn said(text: &str, stop_reason: serde_json::Value) -> String {
        serde_json::json!({
            "type": "assistant", "sessionId": "S1", "timestamp": "2026-11-01T00:00:01.000Z",
            "message": {"role": "assistant", "model": "m", "stop_reason": stop_reason,
                        "content": [{"type": "text", "text": text}]}
        })
        .to_string()
    }

    #[test]
    fn a_last_turn_the_model_has_not_stopped_has_no_ending() {
        // Read while it runs, the text so far is not how the turn ended: 321 of
        // 1,954 real last turns stop on a tool call. `Stop` writes the ending
        // when there is one.
        for stop_reason in [
            serde_json::json!("tool_use"),
            serde_json::Value::Null,
            serde_json::json!("pause_turn"),
        ] {
            let out = parse(&[
                user("", "P1", "a prompt"),
                said("let me look at that first", stop_reason.clone()),
            ]);

            assert_eq!(prompts(&out).len(), 1);
            assert_eq!(endings(&out), Vec::<String>::new(), "{stop_reason}");
            assert_eq!(
                out.skipped.get("reply of a turn not known to have ended"),
                Some(&1)
            );
        }
    }

    #[test]
    fn a_last_turn_the_model_finished_has_its_ending() {
        let out = parse(&[
            user("", "P1", "a prompt"),
            said("the answer", serde_json::json!("end_turn")),
        ]);

        assert_eq!(endings(&out), vec!["the answer"]);
    }

    #[test]
    fn a_turn_cut_short_is_not_recovered_as_complete() {
        // `max_tokens` reaches the live path as `StopFailure`, which is not
        // `Complete`, and the text is truncated. The others do not say it finished
        // either, and a missing ending can be filled by a hook where a false one
        // cannot be taken back.
        for stop_reason in ["max_tokens", "refusal", "stop_sequence"] {
            let out = parse(&[
                user("", "P1", "a prompt"),
                said("the answer so far", serde_json::json!(stop_reason)),
                user("", "P2", "another prompt"),
            ]);

            assert_eq!(endings(&out), Vec::<String>::new(), "{stop_reason}");
        }
    }

    #[test]
    fn the_last_line_of_a_turn_says_whether_it_stopped() {
        // A tool call after the text keeps the turn running, and the model's
        // next stop ends it even when that line has no text of its own.
        let tool_call = r#"{"type":"assistant","sessionId":"S1","timestamp":"2026-11-01T00:00:02.000Z","message":{"role":"assistant","model":"m","stop_reason":"tool_use","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}"#;
        let stopped_without_text = r#"{"type":"assistant","sessionId":"S1","timestamp":"2026-11-01T00:00:03.000Z","message":{"role":"assistant","model":"m","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"done"}]}}"#;

        let running = parse(&[
            user("", "P1", "a prompt"),
            said("the answer", serde_json::json!("end_turn")),
            tool_call.to_string(),
        ]);
        assert_eq!(endings(&running), Vec::<String>::new());

        let finished = parse(&[
            user("", "P1", "a prompt"),
            said("the answer", serde_json::json!("tool_use")),
            stopped_without_text.to_string(),
        ]);
        assert_eq!(endings(&finished), vec!["the answer"]);
    }

    #[test]
    fn a_turn_cut_off_on_a_tool_call_gets_no_ending_from_the_next_prompt() {
        let out = parse(&[
            user("", "P1", "a prompt"),
            said("working on it", serde_json::json!("tool_use")),
            user("", "P2", "another prompt"),
            said("the answer", serde_json::json!("end_turn")),
        ]);

        assert_eq!(prompts(&out).len(), 2);
        assert_eq!(endings(&out), vec!["the answer"]);
    }

    #[test]
    fn a_transcript_that_never_says_why_is_ended_only_by_the_next_prompt() {
        // Lines from a build that wrote no `stop_reason`: the next prompt shows
        // the turn is over, and nothing shows how the last one went.
        let reply = |text: &str| {
            format!(
                r#"{{"type":"assistant","sessionId":"S1","timestamp":"2026-11-01T00:00:01.000Z","message":{{"role":"assistant","content":[{{"type":"text","text":"{text}"}}]}}}}"#
            )
        };
        let out = parse(&[
            user("", "P1", "first"),
            reply("first answer"),
            user("", "P2", "second"),
            reply("second answer"),
        ]);

        assert_eq!(endings(&out), vec!["first answer"]);
    }

    #[test]
    fn a_line_that_names_no_session_is_read_as_the_files_own() {
        // Nothing says it belongs elsewhere, and bookkeeping lines of this
        // shape are common.
        let out = parse_claude_transcript(
            r#"{"type":"user","timestamp":"2026-11-01T00:00:00.000Z","promptId":"P1","message":{"role":"user","content":"a prompt"}}"#,
            "S1",
            BusId::mint(),
        );

        assert_eq!(turns(&out).len(), 1);
    }
}

#[cfg(test)]
mod codex_tests {
    use super::*;

    /// Shaped from the real app-server on 2026-09-03: a `thread/list` item
    /// (timestamps are strings there) and one `thread/turns/list` turn
    /// (timestamps are numbers there), for the throwaway checkout's session.
    fn thread() -> serde_json::Value {
        serde_json::json!({
            "id": "01a06316-03dc-7c31-abae-aa2c2606cdba",
            "sessionId": "01a06316-03dc-7c31-abae-aa2c2606cdba",
            "cwd": "/Applications/conbus-hooktest", "name": "Check conbus-record context",
            "preview": "Is there a conbus-record block in your context?",
            "createdAt": "1788368782", "updatedAt": "1788368799", "recencyAt": "1788368799",
            "historyMode": "paginated", "source": "cli", "cliVersion": "0.152.1",
            "status": "completed", "turns": [], "gitInfo": null, "forkedFromId": null
        })
    }

    fn turns() -> Vec<serde_json::Value> {
        vec![
            serde_json::json!({
                "id": "01a06316-3a98-7022-b592-f9e9ac2305b5",
                "items": [
                    {"type": "userMessage", "id": "01a06316-3d9c-7e01-bab7-2b6046c367a9",
                     "content": [{"type": "text", "text": "Is there a conbus-record block in your context?", "text_elements": []}]},
                    {"type": "agentMessage", "id": "msg_0f41", "text": "Let me look.", "phase": "commentary"},
                    {"type": "agentMessage", "id": "msg_0f42", "text": "YES-RECORD", "phase": "final_answer"}
                ],
                "itemsView": "summary", "status": "completed", "error": null,
                "startedAt": 1788368796, "completedAt": 1788368799, "durationMs": 3242
            }),
            serde_json::json!({
                "id": "01a06316-4b00-7000-8000-000000000002",
                "items": [{"type": "userMessage", "id": "u2", "content": [{"type": "text", "text": "and again"}]}],
                "itemsView": "summary", "status": "inProgress", "error": null,
                "startedAt": 1788368900, "completedAt": null, "durationMs": null
            }),
        ]
    }

    #[test]
    fn a_thread_yields_a_session_its_turns_and_their_endings() {
        let out = parse_codex_thread(&thread(), &turns());

        let kinds: Vec<&str> = out
            .entries
            .iter()
            .map(|e| match &e.event {
                Event::SessionCandidate { .. } => "session",
                Event::TurnStarted { .. } => "started",
                Event::TurnEnded { .. } => "ended",
                other => panic!("codex backfill produced {other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["session", "started", "ended", "started"],
            "a turn still in progress has no ending, which is the truth about it"
        );
        assert_eq!(
            out.vendor_session_id.as_deref(),
            Some("01a06316-03dc-7c31-abae-aa2c2606cdba")
        );
        assert_eq!(
            out.workspace.as_deref(),
            Some("/Applications/conbus-hooktest")
        );
        assert_eq!(
            out.entrypoint.as_ref().map(Entrypoint::as_str),
            Some("cli"),
            "the thread's `source` is the vendor's own word for where it ran"
        );
    }

    #[test]
    fn a_turn_ends_with_the_final_answer_not_the_commentary() {
        let out = parse_codex_thread(&thread(), &turns());

        let Event::TurnEnded {
            assistant_message: Some(message),
            state,
            ..
        } = &out.entries[2].event
        else {
            panic!("expected an ended turn, got {:?}", out.entries[2].event);
        };
        assert_eq!(*message, Body::kept("YES-RECORD"));
        assert_eq!(*state, TurnState::Complete);
        assert_eq!(
            out.entries[1]
                .vendor_turn_id
                .as_ref()
                .map(VendorTurnId::as_str),
            Some("01a06316-3a98-7022-b592-f9e9ac2305b5")
        );
    }

    #[test]
    fn every_recovered_codex_record_says_where_it_came_from_and_when() {
        let out = parse_codex_thread(&thread(), &turns());

        assert!(out.entries.iter().all(|e| {
            e.source.backfilled_from.as_deref() == Some(CODEX_THREAD)
                && e.source.harness.as_str() == Harness::CODEX
                && e.source.harness_version.as_deref() == Some("0.152.1")
        }));
        // The session is stamped with the thread's creation, the turn with
        // its own start — the thread's `createdAt` is a string and the
        // turn's `startedAt` a number, and both are unix seconds.
        assert_eq!(
            out.entries[0].observed_at.map(|t| t.to_string()).as_deref(),
            Some("2026-09-02T17:06:22.000Z")
        );
        assert_eq!(
            out.entries[1].observed_at.map(|t| t.to_string()).as_deref(),
            Some("2026-09-02T17:06:36.000Z")
        );
    }

    #[test]
    fn a_turn_older_than_its_thread_is_the_parent_a_fork_inherited() {
        // A forked thread lists its parent's turns as its own, under the parent's
        // turn ids. Both ids are UUIDv7, so they say when they were minted: on real
        // rollouts all 280 inherited turns are older than their fork, and none of
        // 2,356 turns a thread ran itself is older than it.
        let inherited = serde_json::json!({
            "id": "01a06316-03db-7000-8000-000000000001",
            "items": [
                {"type": "userMessage", "id": "u0", "content": [{"type": "text", "text": "the parent's prompt"}]},
                {"type": "agentMessage", "id": "a0", "text": "the parent's reply", "phase": "final_answer"}
            ],
            "status": "completed", "startedAt": 1788368700, "completedAt": 1788368701
        });
        let mut listed = vec![inherited];
        listed.extend(turns());

        let out = parse_codex_thread(&thread(), &listed);

        let prompts: Vec<&Entry> = out
            .entries
            .iter()
            .filter(|e| matches!(e.event, Event::TurnStarted { .. }))
            .collect();
        assert_eq!(prompts.len(), 2, "the thread's own two: {prompts:?}");
        assert!(prompts.iter().all(|e| {
            e.vendor_turn_id.as_ref().map(VendorTurnId::as_str)
                != Some("01a06316-03db-7000-8000-000000000001")
        }));
        assert_eq!(out.skipped.get("turn older than its thread"), Some(&1));
    }

    #[test]
    fn a_turn_whose_id_says_nothing_about_time_is_kept() {
        // Legacy history can number turns any way it likes.
        let mut turn = turns().remove(0);
        turn["id"] = serde_json::json!("legacy-1");

        let out = parse_codex_thread(&thread(), &[turn]);

        assert_eq!(out.skipped.get("turn older than its thread"), None);
        assert!(
            out.entries
                .iter()
                .any(|e| matches!(e.event, Event::TurnStarted { .. }))
        );
    }

    #[test]
    fn a_thread_whose_id_is_not_a_uuid_is_counted_not_recovered() {
        let mut odd = thread();
        odd["id"] = serde_json::json!("not-a-uuid");

        let out = parse_codex_thread(&odd, &turns());

        assert!(out.entries.is_empty());
        assert_eq!(out.skipped.get("thread id is not a uuid"), Some(&1));
    }
    #[test]
    fn a_turn_of_nothing_but_replies_ends_the_prompt_before_it() {
        // Measured on 2026-09-03: of 1,070 turns in the 60 most recent
        // threads, 590 carried only `agentMessage` items — the VS Code
        // extension's legacy history splits one exchange into a prompt turn
        // and one or more reply-only turns. The Claude transcript parser's
        // rule holds here too: a reply belongs to the most recent prompt,
        // and the last reply is the one that ends it.
        let mut turns = turns();
        turns.truncate(1);
        turns.push(serde_json::json!({
            "id": "01a06316-5c00-7000-8000-000000000003",
            "items": [{"type": "agentMessage", "id": "a9", "text": "and here is the real answer", "phase": "final_answer"}],
            "status": "completed", "error": null,
            "startedAt": 1788368900, "completedAt": 1788368950, "durationMs": 50000
        }));

        let out = parse_codex_thread(&thread(), &turns);

        let kinds: Vec<&str> = out
            .entries
            .iter()
            .map(|e| match &e.event {
                Event::SessionCandidate { .. } => "session",
                Event::TurnStarted { .. } => "started",
                Event::TurnEnded { .. } => "ended",
                other => panic!("{other:?}"),
            })
            .collect();
        assert_eq!(
            kinds,
            vec!["session", "started", "ended"],
            "one prompt, one ending"
        );
        let Event::TurnEnded {
            assistant_message: Some(message),
            ..
        } = &out.entries[2].event
        else {
            panic!("expected an ended turn");
        };
        assert_eq!(*message, Body::kept("and here is the real answer"));
        assert_eq!(
            out.entries[2].observed_at.map(|t| t.to_string()).as_deref(),
            Some("2026-09-02T17:09:10.000Z"),
            "the ending is stamped when the last reply completed"
        );
        assert_eq!(out.skipped.get("turn without a user message"), None);
    }

    #[test]
    fn a_reply_before_any_prompt_is_counted_not_attached_to_nothing() {
        let turns = vec![serde_json::json!({
            "id": "01a06316-5c00-7000-8000-000000000004",
            "items": [{"type": "agentMessage", "id": "a0", "text": "hello?", "phase": "final_answer"}],
            "status": "completed", "error": null, "startedAt": 1788368700, "completedAt": 1788368701
        })];

        let out = parse_codex_thread(&thread(), &turns);

        assert!(out.entries.is_empty());
        assert_eq!(out.skipped.get("reply before any prompt"), Some(&1));
    }
}

#[cfg(test)]
mod chat_export_tests {
    use super::*;

    /// Field names from `claude import-conversations` 2.1.263, which reads
    /// the official Claude.ai / Desktop data-export `conversations.json`.
    const EXPORT: &str = r#"{
        "conversations": [{
            "uuid": "00000000-1111-2222-3333-444444444444",
            "name": "a chat",
            "created_at": "2026-09-06T12:00:00.000Z",
            "updated_at": "2026-09-06T12:01:00.000Z",
            "chat_messages": [
                {"uuid": "m1", "sender": "human", "text": "from the chat window", "created_at": "2026-09-06T12:00:00.000Z"},
                {"uuid": "m2", "sender": "assistant", "text": "kept the same conversation", "created_at": "2026-09-06T12:01:00.000Z"}
            ]
        }]
    }"#;

    #[test]
    fn a_chat_export_keeps_the_conversation_uuid_as_claude_chat() {
        let recovered = parse_claude_chat_export(EXPORT);
        assert_eq!(recovered.len(), 1);
        let out = &recovered[0];
        assert_eq!(
            out.vendor_session_id.as_deref(),
            Some("00000000-1111-2222-3333-444444444444")
        );
        assert_eq!(out.entries[0].source.harness.as_str(), Harness::CLAUDE_CHAT);
        assert_eq!(
            out.entries[0].source.backfilled_from.as_deref(),
            Some(CLAUDE_CHAT_EXPORT)
        );
        match &out.entries[1].event {
            Event::TurnStarted { prompt, .. } => {
                assert_eq!(*prompt, Body::kept("from the chat window"));
            }
            other => panic!("{other:?}"),
        }
        match &out.entries[2].event {
            Event::TurnEnded {
                assistant_message: Some(message),
                ..
            } => assert_eq!(*message, Body::kept("kept the same conversation")),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_unknown_export_schema_is_skipped_not_guessed() {
        let recovered = parse_claude_chat_export(r#"{"threads":[]}"#);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].skipped.get("unknown schema"), Some(&1));
        assert!(recovered[0].entries.is_empty());
    }

    /// Privacy → Export data writes the conversation objects as a JSON array.
    const ARRAY_EXPORT: &str = r#"[
        {
            "uuid": "00000000-aaaa-bbbb-cccc-ddddeeeeffff",
            "name": "privacy export",
            "chat_messages": [
                {"uuid": "m1", "sender": "human", "text": "", "content": [{"type": "text", "text": "typed in chat"}], "created_at": "2026-09-06T12:00:00.000Z"},
                {"uuid": "m2", "sender": "assistant", "text": "", "content": [{"type": "text", "text": "same uuid"}], "created_at": "2026-09-06T12:01:00.000Z"}
            ]
        }
    ]"#;

    #[test]
    fn a_privacy_export_array_keeps_the_chat_uuid() {
        let recovered = parse_claude_chat_export(ARRAY_EXPORT);
        assert_eq!(recovered.len(), 1);
        let out = &recovered[0];
        assert_eq!(
            out.vendor_session_id.as_deref(),
            Some("00000000-aaaa-bbbb-cccc-ddddeeeeffff")
        );
        assert_eq!(out.entries[0].source.harness.as_str(), Harness::CLAUDE_CHAT);
        match &out.entries[1].event {
            Event::TurnStarted { prompt, .. } => {
                assert_eq!(*prompt, Body::kept("typed in chat"));
            }
            other => panic!("{other:?}"),
        }
        match &out.entries[2].event {
            Event::TurnEnded {
                assistant_message: Some(message),
                ..
            } => assert_eq!(*message, Body::kept("same uuid")),
            other => panic!("{other:?}"),
        }
    }
}
