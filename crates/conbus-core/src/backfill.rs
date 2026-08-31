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

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::clock::Timestamp;
use crate::event::{Author, Body, Event, Harness, Source, TurnState, VendorTurnId};
use crate::ids::BusId;
use crate::record::Entry;

/// Names the shape this parser read, so a record can say where it came from.
pub const CLAUDE_TRANSCRIPT: &str = "claude_transcript";

/// What one transcript yielded.
#[derive(Debug, Default)]
pub struct Recovered {
    /// The vendor's session id, from the lines themselves.
    pub vendor_session_id: Option<String>,
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
    #[serde(default)]
    message: Option<Message>,
}

#[derive(Deserialize)]
struct Message {
    #[serde(default)]
    content: Option<serde_json::Value>,
}

/// A turn that has started and not yet been closed by the next prompt.
struct OpenTurn {
    /// The most recent reply in this turn: when it came, what it said, and the
    /// stamp of the line that carried it. Only the last one survives, matching
    /// what `Stop` sends on the live path. The stamp is the reply's own — a
    /// vendor upgrade mid-conversation is ordinary, and the ending belongs to
    /// the build that wrote it, not to the one that read the prompt.
    last_reply: Option<(Option<Timestamp>, String, Source)>,
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
pub fn parse_claude_transcript(text: &str, bus_id: BusId) -> Recovered {
    let mut out = Recovered::default();
    let mut open: Option<OpenTurn> = None;

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
        if out.vendor_session_id.is_none() {
            out.vendor_session_id.clone_from(&line.session_id);
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
            backfilled_from: Some(CLAUDE_TRANSCRIPT.to_string()),
        };
        let kind = line.r#type.as_deref().unwrap_or("(none)");

        match kind {
            "user" => {
                let Some(text) = line
                    .message
                    .as_ref()
                    .and_then(|m| m.content.as_ref())
                    .and_then(text_of)
                else {
                    out.skip("user line with no text");
                    continue;
                };
                close(&mut out, bus_id, open.take());
                out.entries.push(Entry {
                    bus_id,
                    observed_at: at,
                    source: source.clone(),
                    vendor_turn_id: line.prompt_id.clone().map(VendorTurnId::new),
                    vendor_agent_id: None,
                    event: Event::TurnStarted {
                        author: line.prompt_source.clone().map(Author::new),
                        prompt: Body::kept(text),
                    },
                });
                open = Some(OpenTurn { last_reply: None });
            }
            "assistant" => {
                let Some(text) = line
                    .message
                    .as_ref()
                    .and_then(|m| m.content.as_ref())
                    .and_then(text_of)
                else {
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
    close(&mut out, bus_id, open.take());
    out
}

/// End a turn with the last reply it received. A prompt that was never answered
/// simply has no ending in the transcript, which is the truth about it.
fn close(out: &mut Recovered, bus_id: BusId, open: Option<OpenTurn>) {
    let Some(OpenTurn {
        last_reply: Some((at, text, source)),
    }) = open
    else {
        return;
    };
    out.entries.push(Entry {
        bus_id,
        observed_at: at,
        source,
        vendor_turn_id: None,
        vendor_agent_id: None,
        event: Event::TurnEnded {
            state: TurnState::Complete,
            assistant_message: Some(Body::kept(text)),
        },
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped from real transcripts: a user line carries `promptId`,
    /// `promptSource`, `version` and a bare-string content; an assistant line
    /// carries a block list and no `promptId` at all.
    const TRANSCRIPT: &str = r#"
{"type":"queue-operation","sessionId":"S1","timestamp":"2026-07-28T15:36:40.000Z"}
{"type":"user","sessionId":"S1","timestamp":"2026-07-28T15:36:42.867Z","version":"2.1.229","promptId":"P1","message":{"role":"user","content":"first question"}}
{"type":"attachment","sessionId":"S1","subtype":"hook_success"}
{"type":"assistant","sessionId":"S1","timestamp":"2026-07-28T15:36:50.000Z","version":"2.1.229","message":{"role":"assistant","content":[{"type":"text","text":"thinking about it"}]}}
{"type":"assistant","sessionId":"S1","timestamp":"2026-07-28T15:36:56.388Z","version":"2.1.229","message":{"role":"assistant","content":[{"type":"tool_use","id":"t1","name":"Read","input":{}}]}}
{"type":"assistant","sessionId":"S1","timestamp":"2026-07-28T15:37:00.000Z","version":"2.1.229","message":{"role":"assistant","content":[{"type":"text","text":"the answer"}]}}
{"type":"user","sessionId":"S1","timestamp":"2026-07-28T15:38:00.000Z","version":"2.1.229","promptId":"P2","promptSource":"sdk","message":{"role":"user","content":[{"type":"text","text":"second question"}]}}
{"type":"assistant","sessionId":"S1","timestamp":"2026-07-28T15:38:10.000Z","version":"2.1.247","message":{"role":"assistant","content":[{"type":"text","text":"and the second answer"}]}}
"#;

    fn parsed() -> Recovered {
        parse_claude_transcript(TRANSCRIPT, BusId::mint())
    }

    #[test]
    fn a_transcript_yields_its_turns_and_nothing_else() {
        let out = parsed();

        let kinds: Vec<&str> = out
            .entries
            .iter()
            .map(|e| match &e.event {
                Event::TurnStarted { .. } => "started",
                Event::TurnEnded { .. } => "ended",
                other => panic!("backfill produced {other:?}"),
            })
            .collect();
        assert_eq!(kinds, vec!["started", "ended", "started", "ended"]);
        assert_eq!(out.vendor_session_id.as_deref(), Some("S1"));
    }

    #[test]
    fn a_turn_ends_once_with_its_last_reply() {
        // Three assistant lines answer the first prompt: text, a tool call, and
        // the closing text. One turn, one ending, and the ending is the last
        // text — the same thing `Stop.last_assistant_message` carries live.
        let out = parsed();

        let Event::TurnEnded {
            assistant_message: Some(message),
            ..
        } = &out.entries[1].event
        else {
            panic!("expected an ended turn, got {:?}", out.entries[1].event);
        };
        assert_eq!(*message, Body::kept("the answer"));
    }

    #[test]
    fn the_author_the_live_path_never_sees_is_recovered_here() {
        // `UserPromptSubmit.source` is compiled out of the hook payload, so the
        // transcript's `promptSource` is the only place a turn's author is
        // recorded at all. Absent stays absent — never read as human.
        let out = parsed();

        let Event::TurnStarted { author, .. } = &out.entries[0].event else {
            panic!("expected a started turn");
        };
        assert_eq!(*author, None);
        let Event::TurnStarted { author, .. } = &out.entries[2].event else {
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
    fn the_vendor_version_the_hook_never_sends_is_recovered_here() {
        let out = parsed();

        assert_eq!(
            out.entries[0].source.harness_version.as_deref(),
            Some("2.1.229")
        );
        assert_eq!(
            out.entries[3].source.harness_version.as_deref(),
            Some("2.1.247")
        );
    }

    #[test]
    fn the_prompt_correlator_is_recovered_and_replies_carry_none() {
        let out = parsed();

        assert_eq!(
            out.entries[0]
                .vendor_turn_id
                .as_ref()
                .map(VendorTurnId::as_str),
            Some("P1")
        );
        assert_eq!(
            out.entries[1].vendor_turn_id, None,
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

        let out = parse_claude_transcript(&damaged, BusId::mint());

        assert_eq!(out.entries.len(), 4, "the conversation must survive");
        assert_eq!(out.skipped.get("unreadable json"), Some(&1));
    }

    #[test]
    fn subagent_traffic_is_left_out_of_the_main_conversation() {
        let with_sidechain = format!(
            "{TRANSCRIPT}\n{}",
            r#"{"type":"user","sessionId":"S1","isSidechain":true,"timestamp":"2026-07-28T15:39:00.000Z","promptId":"P9","message":{"role":"user","content":"a subagent prompt"}}"#
        );

        let out = parse_claude_transcript(&with_sidechain, BusId::mint());

        assert_eq!(out.entries.len(), 4);
        assert_eq!(out.skipped.get("sidechain"), Some(&1));
    }
}
