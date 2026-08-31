//! What a workspace is willing to leave on disk.

use crate::event::{Body, Event, ToolOutcome};
use crate::record::Record;

/// Rebuild an event with every body passed through `f`.
fn map_bodies(event: &Event, f: impl Fn(&Body) -> Body) -> Event {
    match event {
        // Spelled out rather than `..`: silencing this with `author: None`
        // would erase the author at every retention level, on the only path to
        // disk, and every existing test would still pass.
        Event::TurnStarted { author, prompt } => Event::TurnStarted {
            author: author.clone(),
            prompt: f(prompt),
        },
        // Spelled out rather than `..`: retention withholds bodies, and
        // silencing this arm by dropping the identity or the timing would
        // withhold metadata at every level, including `Full`.
        Event::ToolRequested {
            tool,
            tool_use_id,
            input,
        } => Event::ToolRequested {
            tool: tool.clone(),
            tool_use_id: tool_use_id.clone(),
            input: f(input),
        },
        Event::ToolDenied {
            tool,
            tool_use_id,
            input,
            reason,
        } => Event::ToolDenied {
            tool: tool.clone(),
            tool_use_id: tool_use_id.clone(),
            input: f(input),
            reason: reason.clone(),
        },
        Event::ToolUse {
            tool,
            tool_use_id,
            duration_ms,
            input,
            outcome,
        } => Event::ToolUse {
            tool: tool.clone(),
            tool_use_id: tool_use_id.clone(),
            duration_ms: *duration_ms,
            input: f(input),
            // A failure's error text is tool output too — a failed shell
            // command puts its stdout there — so it goes through the same
            // filter as a successful response.
            outcome: match outcome {
                ToolOutcome::Succeeded { output } => ToolOutcome::Succeeded { output: f(output) },
                ToolOutcome::Failed { error, interrupted } => ToolOutcome::Failed {
                    error: f(error),
                    interrupted: *interrupted,
                },
            },
        },
        Event::TurnEnded {
            state,
            assistant_message,
        } => Event::TurnEnded {
            state: state.clone(),
            assistant_message: assistant_message.as_ref().map(&f),
        },
        other => other.clone(),
    }
}

/// Replace tool result bodies with their size, leaving conversation alone.
fn elide_tool_bodies(event: &Event) -> Event {
    match event {
        Event::ToolRequested { .. } | Event::ToolDenied { .. } | Event::ToolUse { .. } => {
            map_bodies(event, |body| Body::Elided {
                bytes: body.original_bytes(),
            })
        }
        // Exhaustive for the same reason as `map_bodies`: a new event carrying
        // tool output must not reach disk at the default level just because
        // nobody remembered to add it here.
        Event::TurnStarted { .. }
        | Event::TurnEnded { .. }
        | Event::SessionCandidate { .. }
        | Event::ToolBatchEnded { .. }
        | Event::SessionEnded { .. } => event.clone(),
    }
}

fn clip_bodies(event: &Event) -> Event {
    map_bodies(event, clip)
}

fn clip(body: &Body) -> Body {
    let Body::Kept { text } = body else {
        return body.clone();
    };
    if text.len() <= INLINE_CAP_BYTES {
        return body.clone();
    }
    // Walk back to a character boundary. At most three bytes, since that is the
    // widest a UTF-8 continuation run can be behind the cap.
    let mut end = INLINE_CAP_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    Body::Clipped {
        text: text[..end].to_string(),
        original_bytes: text.len(),
    }
}

/// The largest body that goes inline in a log record. Anything past this is
/// clipped; the full text belongs in the archive, not the segment.
pub const INLINE_CAP_BYTES: usize = 16 * 1024;

/// Per-workspace retention. The axis is one thing: what does not reach disk.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Retention {
    /// Conversation plus tool result bodies. Opt-in.
    Full,
    /// Conversation only. Tool result bodies never reach disk.
    #[default]
    ConversationOnly,
    /// Nothing reaches disk.
    Ephemeral,
}

impl Retention {
    /// The record as it should reach disk, or `None` if nothing should.
    pub fn redact(&self, record: &Record) -> Option<Record> {
        match self {
            Retention::Full => Some(Record {
                event: clip_bodies(&record.event),
                ..record.clone()
            }),
            Retention::Ephemeral => None,
            Retention::ConversationOnly => Some(Record {
                event: clip_bodies(&elide_tool_bodies(&record.event)),
                ..record.clone()
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{Author, Body, Event, Harness, Source, ToolOutcome, VendorToolUseId};
    use crate::record::SCHEMA_VERSION;

    fn tool_use(input: Body, output: Body) -> Record {
        Record {
            schema_version: SCHEMA_VERSION,
            seq: 1,
            ts: "2026-08-30T07:41:02.481Z".parse().expect("ts"),
            bus_id: "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e".parse().expect("id"),
            source: Source {
                harness: Harness::new(Harness::CLAUDE_CODE),
                harness_version: None,
                vendor_session_id: None,
                workspace: None,
                entrypoint: None,
                backfilled_from: None,
            },
            vendor_turn_id: None,
            vendor_agent_id: None,
            event: Event::ToolUse {
                tool: "Read".into(),
                tool_use_id: None,
                duration_ms: None,
                input,
                outcome: ToolOutcome::Succeeded { output },
            },
        }
    }

    fn turn_started(prompt: Body) -> Record {
        Record {
            event: Event::TurnStarted {
                author: None,
                prompt,
            },
            ..tool_use(Body::kept(""), Body::kept(""))
        }
    }

    #[test]
    fn a_body_over_the_inline_cap_is_clipped_and_says_how_big_it_was() {
        let huge = "x".repeat(INLINE_CAP_BYTES + 500);

        let redacted = Retention::default()
            .redact(&turn_started(Body::kept(&huge)))
            .expect("a prompt is logged");

        let Event::TurnStarted { prompt, .. } = &redacted.event else {
            panic!("the event kind must survive redaction");
        };
        assert_eq!(
            *prompt,
            Body::Clipped {
                text: "x".repeat(INLINE_CAP_BYTES),
                original_bytes: INLINE_CAP_BYTES + 500,
            }
        );
    }

    #[test]
    fn clipping_never_splits_a_character() {
        // A cap that lands mid-character is the interesting case: 'ㅎ' is three
        // bytes, so the cap boundary falls inside one of them.
        let korean = "한".repeat(INLINE_CAP_BYTES);

        let redacted = Retention::default()
            .redact(&turn_started(Body::kept(&korean)))
            .expect("a prompt is logged");

        let Event::TurnStarted {
            prompt:
                Body::Clipped {
                    text,
                    original_bytes,
                },
            ..
        } = &redacted.event
        else {
            panic!("expected a clipped prompt, got {:?}", redacted.event);
        };
        assert!(text.len() <= INLINE_CAP_BYTES, "clip must respect the cap");
        assert!(
            INLINE_CAP_BYTES - text.len() < 4,
            "clip must not waste a whole character"
        );
        assert_eq!(*original_bytes, korean.len());
    }

    #[test]
    fn a_denials_reason_survives_while_its_input_does_not() {
        // The split this schema draws: tool bodies are withheld by default, and
        // the harness explaining itself is kept — the same treatment a turn's
        // interruption reason and a session's end reason get. Eliding the
        // reason would delete the only account of why a request never resolved.
        let record = Record {
            event: Event::ToolDenied {
                tool: "Bash".into(),
                tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
                input: Body::kept(r#"{"command":"rm -rf /"}"#),
                reason: "User rejected the command".into(),
            },
            ..tool_use(Body::kept(""), Body::kept(""))
        };

        for level in [Retention::Full, Retention::ConversationOnly] {
            let redacted = level.redact(&record).expect("logged");

            let Event::ToolDenied { input, reason, .. } = &redacted.event else {
                panic!("the event kind must survive redaction");
            };
            assert_eq!(
                reason, "User rejected the command",
                "{level:?} dropped the reason"
            );
            if level == Retention::ConversationOnly {
                assert_eq!(*input, Body::Elided { bytes: 22 }, "the input reached disk");
            }
        }
    }

    #[test]
    fn a_requested_tool_input_is_withheld_by_default_like_any_other_tool_body() {
        // The request carries the same material as the result — a `Write`'s
        // whole file body arrives here first. Mirroring the request must not
        // become a way for tool bodies to reach disk at the level that
        // promises they will not.
        let record = Record {
            event: Event::ToolRequested {
                tool: "Write".into(),
                tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
                input: Body::kept(r#"{"content":"a secret"}"#),
            },
            ..tool_use(Body::kept(""), Body::kept(""))
        };

        let default = Retention::default().redact(&record).expect("logged");
        let full = Retention::Full.redact(&record).expect("logged");

        let Event::ToolRequested {
            input, tool_use_id, ..
        } = &default.event
        else {
            panic!("the event kind must survive redaction");
        };
        assert_eq!(*input, Body::Elided { bytes: 22 });
        assert_eq!(
            tool_use_id.as_ref().map(VendorToolUseId::as_str),
            Some("toolu_01ABC"),
            "the join key is metadata and must survive"
        );
        assert_eq!(full.event, record.event, "full retention keeps it verbatim");
    }

    #[test]
    fn a_failures_error_text_is_a_tool_body_and_is_withheld_by_default() {
        // A failed shell command puts its output in `error`, so the same
        // reasoning that keeps tool responses off disk applies to it. Keeping
        // it because "it is only an error" would put the sensitive half of a
        // failed command on disk at the level that promises not to.
        let record = Record {
            event: Event::ToolUse {
                tool: "Bash".into(),
                tool_use_id: None,
                duration_ms: None,
                input: Body::kept("in"),
                outcome: ToolOutcome::Failed {
                    error: Body::kept("secret.env: permission denied"),
                    interrupted: true,
                },
            },
            ..tool_use(Body::kept(""), Body::kept(""))
        };

        let default = Retention::default().redact(&record).expect("logged");
        let full = Retention::Full.redact(&record).expect("logged");

        let Event::ToolUse { outcome, .. } = &default.event else {
            panic!("the event kind must survive redaction");
        };
        assert_eq!(
            *outcome,
            ToolOutcome::Failed {
                error: Body::Elided { bytes: 29 },
                interrupted: true,
            },
            "the error text reached disk at the level that promises it will not"
        );
        assert_eq!(full.event, record.event, "full retention keeps it verbatim");
    }

    #[test]
    fn redaction_keeps_a_tool_calls_identity_and_duration() {
        // Retention withholds tool *bodies*. The identity and the timing are
        // metadata, and metadata is what keeps an elided log useful — the same
        // reason `Body::Elided` keeps its byte count. Silencing the compiler
        // here with `..` or `None` would drop both at every level.
        let record = Record {
            event: Event::ToolUse {
                tool: "Read".into(),
                tool_use_id: Some(VendorToolUseId::new("toolu_01ABC")),
                duration_ms: Some(37),
                input: Body::kept("in"),
                outcome: ToolOutcome::Succeeded {
                    output: Body::kept("out"),
                },
            },
            ..tool_use(Body::kept(""), Body::kept(""))
        };

        for level in [Retention::Full, Retention::ConversationOnly] {
            let redacted = level.redact(&record).expect("a tool use is logged");

            let Event::ToolUse {
                tool_use_id,
                duration_ms,
                ..
            } = &redacted.event
            else {
                panic!("the event kind must survive redaction");
            };
            assert_eq!(
                tool_use_id.as_ref().map(VendorToolUseId::as_str),
                Some("toolu_01ABC"),
                "{level:?} dropped the tool use id"
            );
            assert_eq!(*duration_ms, Some(37), "{level:?} dropped the duration");
        }
    }

    #[test]
    fn redaction_keeps_the_author_of_a_turn() {
        // Retention governs content, not attribution. The compiler forces this
        // arm to be rewritten when `author` is added, and the tempting fix —
        // writing `author: None` — would erase it at every level including
        // `Full`, on the only path to disk, with every other test still green.
        let record = Record {
            event: Event::TurnStarted {
                author: Some(Author::new(Author::SDK)),
                prompt: Body::kept("wake up and check the deploy"),
            },
            ..turn_started(Body::kept(""))
        };

        for level in [Retention::Full, Retention::ConversationOnly] {
            let redacted = level.redact(&record).expect("a prompt is logged");

            let Event::TurnStarted { author, .. } = &redacted.event else {
                panic!("the event kind must survive redaction");
            };
            assert_eq!(
                author.as_ref().map(Author::as_str),
                Some(Author::SDK),
                "{level:?} dropped the author"
            );
        }
    }

    #[test]
    fn the_default_level_still_keeps_the_conversation() {
        let record = turn_started(Body::kept("ship the recorder"));

        let redacted = Retention::default()
            .redact(&record)
            .expect("a prompt is logged");

        assert_eq!(
            redacted.event,
            Event::TurnStarted {
                author: None,
                prompt: Body::kept("ship the recorder")
            }
        );
    }

    #[test]
    fn full_retention_writes_tool_bodies_verbatim() {
        let record = tool_use(Body::kept("in"), Body::kept("out"));

        let redacted = Retention::Full
            .redact(&record)
            .expect("a tool use is logged");

        assert_eq!(redacted, record);
    }

    #[test]
    fn ephemeral_retention_writes_nothing_at_all() {
        assert_eq!(
            Retention::Ephemeral.redact(&turn_started(Body::kept("secret"))),
            None
        );
        assert_eq!(
            Retention::Ephemeral.redact(&tool_use(Body::kept("in"), Body::kept("out"))),
            None
        );
    }

    #[test]
    fn the_default_level_keeps_tool_bodies_off_disk_but_records_their_size() {
        let record = tool_use(
            Body::kept("{\"path\":\"/etc/hosts\"}"),
            Body::kept("127.0.0.1 localhost"),
        );

        let redacted = Retention::default()
            .redact(&record)
            .expect("a tool use is still logged");

        let Event::ToolUse { input, outcome, .. } = &redacted.event else {
            panic!("the event kind must survive redaction");
        };
        assert_eq!(*input, Body::Elided { bytes: 21 });
        assert_eq!(
            *outcome,
            ToolOutcome::Succeeded {
                output: Body::Elided { bytes: 19 }
            }
        );
    }
}
