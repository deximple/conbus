//! Official Anthropic Inference hooks prompt frame.
//!
//! Enterprise orgs point an HTTPS terminator at `conbus hook`. Anthropic
//! POSTs the conversation transcript before each governed inference —
//! claude.ai Chat, Cowork, and Claude Code — and waits for an allow/deny
//! verdict. Desktop Chat still hard-disables plugin hooks; this is the
//! official live UUID+turns channel for that window.
//!
//! Schema: platform.claude.com/docs/en/manage-claude/inference-hooks-endpoint
//! We archive (`{"action":"allow"}`) and never deny. Actor email is discarded.

use serde::Deserialize;
use serde_json::Value;

use conbus_core::event::{Body, Event, Harness, VendorTurnId};
use conbus_core::registry::{Observed, SessionKey};

use super::{HookError, Normalized};

/// The verdict Anthropic holds the Chat turn for. Deny would block the window.
pub const ALLOW: &str = r#"{"action":"allow"}"#;

/// Official docs example, `source.application = "claude-ai"`.
#[derive(Debug, Deserialize)]
struct Frame {
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    request_id: Option<String>,
    #[serde(default)]
    session_id: Option<String>,
    #[serde(default)]
    source: Option<Source>,
    #[serde(default)]
    messages: Vec<Message>,
}

#[derive(Debug, Deserialize)]
struct Source {
    #[serde(default)]
    application: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Message {
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    content: Vec<Block>,
}

#[derive(Debug, Deserialize)]
struct Block {
    #[serde(default, rename = "type")]
    kind: Option<String>,
    #[serde(default)]
    text: Option<String>,
}

/// Distinct from every vendor hook we already read: `type` is the event,
/// `source.application` names the surface, `messages` is the transcript.
pub fn looks_like(payload: &str) -> bool {
    let Ok(Value::Object(fields)) = serde_json::from_str::<Value>(payload) else {
        return false;
    };
    fields.get("type").and_then(Value::as_str) == Some("prompt")
        && fields
            .get("source")
            .and_then(Value::as_object)
            .and_then(|source| source.get("application"))
            .and_then(Value::as_str)
            .is_some_and(|app| !app.is_empty())
        && fields.get("messages").map(Value::is_array).unwrap_or(false)
}

pub fn normalize(payload: &str) -> Result<Normalized, HookError> {
    let frame: Frame = serde_json::from_str(payload).map_err(|err| HookError(err.to_string()))?;
    if frame.kind != "prompt" {
        return Ok(Normalized::Ignored {
            hook_event_name: frame.kind,
        });
    }
    let application = frame
        .source
        .as_ref()
        .and_then(|source| source.application.as_deref())
        .unwrap_or("");
    // Connection test and Code (already hooked) are not the Chat window.
    if application == "config-test" || application == "claude-code" {
        return Ok(Normalized::Ignored {
            hook_event_name: "prompt".into(),
        });
    }
    let Some(session) = frame
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|id| !id.is_empty())
    else {
        return Ok(Normalized::Ignored {
            hook_event_name: "prompt".into(),
        });
    };
    let Some(prompt) = last_user_text(&frame.messages) else {
        return Ok(Normalized::Ignored {
            hook_event_name: "prompt".into(),
        });
    };
    Ok(Normalized::Event {
        key: SessionKey::new(Harness::new(Harness::CLAUDE_CHAT), session.to_string()),
        hook_event_name: "prompt".into(),
        observed: Box::new(Observed {
            vendor_turn_id: frame
                .request_id
                .filter(|id| !id.is_empty())
                .map(VendorTurnId::new),
            vendor_agent_id: None,
            workspace: None,
            event: Event::TurnStarted {
                author: None,
                prompt: Body::kept(prompt),
            },
        }),
    })
}

fn last_user_text(messages: &[Message]) -> Option<String> {
    messages.iter().rev().find_map(|message| {
        if message.role.as_deref() == Some("user") {
            first_text(&message.content)
        } else {
            None
        }
    })
}

fn first_text(blocks: &[Block]) -> Option<String> {
    blocks.iter().find_map(|block| {
        (block.kind.as_deref() == Some("text"))
            .then(|| block.text.as_deref())
            .flatten()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_string)
    })
}

/// Official Inference hooks endpoint. Anthropic POSTs the prompt frame and
/// waits for a verdict. Always allow; a deny would block the Chat window.
///
/// The org's HTTPS terminator points here (`127.0.0.1:8787` behind TLS).
/// No new crate — one request at a time, same record path as stdin.
pub fn serve(addr: &str, harness: &str) -> std::io::Result<()> {
    let listener = std::net::TcpListener::bind(addr)?;
    eprintln!(
        "conbus hook: inference listen {} — POST official prompt frames, always allow",
        listener.local_addr()?
    );
    for incoming in listener.incoming() {
        let Ok(mut stream) = incoming else {
            continue;
        };
        let _ = handle_http(&mut stream, harness);
    }
    Ok(())
}

fn handle_http(
    stream: &mut (impl std::io::Read + std::io::Write),
    harness: &str,
) -> std::io::Result<()> {
    let body = read_http_body(stream).unwrap_or_default();
    let reply = match crate::hook::client::run(harness, &mut body.as_bytes()) {
        Ok(reply) => reply,
        Err(err) => {
            if crate::drops::record(&err) {
                eprintln!("conbus hook: record dropped: {err}");
            }
            crate::hook::client::failure_reply(harness, &body).to_string()
        }
    };
    write_http_ok(stream, &reply)
}

fn read_http_body(stream: &mut impl std::io::Read) -> std::io::Result<String> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let n = stream.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some((headers, rest)) = split_headers(&buf) {
            let want = content_length(headers).unwrap_or(0).min(10 * 1024 * 1024);
            let mut body = rest.to_vec();
            while body.len() < want {
                let n = stream.read(&mut chunk)?;
                if n == 0 {
                    break;
                }
                body.extend_from_slice(&chunk[..n]);
            }
            body.truncate(want);
            return Ok(String::from_utf8_lossy(&body).into_owned());
        }
        if buf.len() > 64 * 1024 {
            break;
        }
    }
    Ok(String::new())
}

fn split_headers(buf: &[u8]) -> Option<(&str, &[u8])> {
    let pos = buf.windows(4).position(|w| w == b"\r\n\r\n")?;
    let headers = std::str::from_utf8(&buf[..pos]).ok()?;
    Some((headers, &buf[pos + 4..]))
}

fn content_length(headers: &str) -> Option<usize> {
    headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse().ok())
            .flatten()
    })
}

fn write_http_ok(stream: &mut impl std::io::Write, body: &str) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    stream.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hook::normalize;
    use conbus_core::event::Event;

    const OFFICIAL: &str = include_str!("../../tests/fixtures/claude_inference_hook_prompt.json");

    #[test]
    fn the_official_claude_ai_frame_attaches_as_claude_chat() {
        match normalize(&Harness::new(Harness::CLAUDE_CODE), OFFICIAL).expect("ok") {
            Normalized::Event {
                key,
                hook_event_name,
                observed,
            } => {
                assert_eq!(key.harness.as_str(), Harness::CLAUDE_CHAT);
                assert_eq!(
                    key.vendor_session_id,
                    "22222222-2222-2222-2222-222222222222"
                );
                assert_eq!(hook_event_name, "prompt");
                assert_eq!(
                    observed.vendor_turn_id.as_ref().map(|id| id.as_str()),
                    Some("req_abc123")
                );
                assert_eq!(
                    observed.event,
                    Event::TurnStarted {
                        author: None,
                        prompt: Body::kept("Summarize the attached report.")
                    }
                );
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_config_test_is_ignored() {
        let json = r#"{
            "type":"prompt",
            "request_id":"req_test",
            "source":{"application":"config-test"},
            "session_id":null,
            "messages":[]
        }"#;
        match normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("ok") {
            Normalized::Ignored { hook_event_name } => assert_eq!(hook_event_name, "prompt"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_code_frame_is_not_stolen_as_chat() {
        let json = r#"{
            "type":"prompt",
            "request_id":"req_code",
            "source":{"application":"claude-code"},
            "session_id":"aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee",
            "messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]
        }"#;
        match normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("ok") {
            Normalized::Ignored { .. } => {}
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_claude_code_hook_is_not_stolen() {
        let json = r#"{"session_id":"s","hook_event_name":"UserPromptSubmit","prompt":"hi","prompt_id":"T"}"#;
        assert!(!looks_like(json));
        match normalize(&Harness::new(Harness::CLAUDE_CODE), json).expect("ok") {
            Normalized::Event { key, .. } => {
                assert_eq!(key.harness.as_str(), Harness::CLAUDE_CODE);
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn an_http_post_of_the_official_frame_allows() {
        let body = include_str!("../../tests/fixtures/claude_inference_hook_prompt.json");
        let request = format!(
            "POST / HTTP/1.1\r\nHost: conbus\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        );
        let mut stream = Mock {
            read: std::io::Cursor::new(request.into_bytes()),
            write: Vec::new(),
        };
        handle_http(&mut stream, Harness::CLAUDE_CODE).expect("http");
        let reply = String::from_utf8(stream.write).expect("utf8");
        assert!(reply.starts_with("HTTP/1.1 200 OK"), "{reply}");
        assert!(reply.contains(ALLOW), "{reply}");
    }

    struct Mock {
        read: std::io::Cursor<Vec<u8>>,
        write: Vec<u8>,
    }

    impl std::io::Read for Mock {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.read, buf)
        }
    }

    impl std::io::Write for Mock {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            std::io::Write::write(&mut self.write, buf)
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn actor_email_is_not_kept() {
        assert!(
            !OFFICIAL.contains("alice@example.com")
                || normalize(&Harness::new(Harness::CLAUDE_CODE), OFFICIAL)
                    .ok()
                    .is_some_and(|n| format!("{n:?}").contains("claude_chat"))
        );
        let dumped = format!(
            "{:?}",
            normalize(&Harness::new(Harness::CLAUDE_CODE), OFFICIAL).expect("ok")
        );
        assert!(
            !dumped.contains("alice@example.com"),
            "email must not land on the bus: {dumped}"
        );
    }
}
