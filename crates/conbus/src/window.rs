//! Open the vendor's own window on a session the bus minted.
//!
//! Codex.app registers `codex://` (Info.plist `com.openai.codex`) and its
//! renderer opens `codex://threads/{id}`. Claude Desktop registers `claude://`
//! and ships `claude://code/continue?session={id}` as a launcher action.
//! Neither call is a chat UI, and neither injects into a thread the user opened.

use std::io;
use std::process::{Command, Stdio};

use conbus_core::event::Harness;

/// Official deep link for a bus-owned warm, when the vendor has one.
pub fn url_for(harness: &str, session: &str) -> Option<String> {
    match harness {
        Harness::CODEX if thread_uuid(session) => Some(format!("codex://threads/{session}")),
        Harness::CLAUDE_CODE if safe_id(session) => {
            Some(format!("claude://code/continue?session={session}"))
        }
        // Desktop allowlist: host claude.ai, path /chat/{uuid}, no query.
        // This keeps Chat identity. `claude://code/…` is a different surface.
        Harness::CLAUDE_CHAT if thread_uuid(session) => {
            Some(format!("claude://claude.ai/chat/{session}"))
        }
        _ => None,
    }
}

/// Bring the vendor app to the bus-owned session. Failure is the caller's
/// to ignore — takeover still execs the TTY.
pub fn reveal(harness: &str, session: &str) -> io::Result<bool> {
    let Some(url) = url_for(harness, session) else {
        return Ok(false);
    };
    let bin = std::env::var_os("CONBUS_OPEN_BIN").unwrap_or_else(|| "open".into());
    let mut cmd = Command::new(bin);
    match harness {
        Harness::CODEX => {
            cmd.args(["-b", "com.openai.codex", "-u"]);
        }
        Harness::CLAUDE_CODE | Harness::CLAUDE_CHAT => {
            cmd.args(["-b", "com.anthropic.claudefordesktop", "-u"]);
        }
        _ => {}
    }
    let status = cmd
        .arg(&url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    Ok(status.success())
}

/// Codex.app's deeplink parser: lowercase UUID only. `warm:` and `thr_bus` fail.
fn thread_uuid(id: &str) -> bool {
    let mut parts = id.split('-');
    matches!(
        (
            parts.next().map(|p| p.len() == 8 && hex(p)),
            parts.next().map(|p| p.len() == 4 && hex(p)),
            parts.next().map(|p| p.len() == 4 && hex(p)),
            parts.next().map(|p| p.len() == 4 && hex(p)),
            parts.next().map(|p| p.len() == 12 && hex(p)),
            parts.next(),
        ),
        (
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            Some(true),
            None
        )
    )
}

fn hex(s: &str) -> bool {
    s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn safe_id(id: &str) -> bool {
    let bytes = id.as_bytes();
    (8..=80).contains(&bytes.len())
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_codex_warm_is_the_app_thread_url() {
        assert_eq!(
            url_for(Harness::CODEX, "01a06307-1111-2222-3333-444444444444").as_deref(),
            Some("codex://threads/01a06307-1111-2222-3333-444444444444")
        );
    }

    #[test]
    fn a_claude_warm_opens_desktop_code_not_chat() {
        assert_eq!(
            url_for(Harness::CLAUDE_CODE, "01a072de-0887-7620-a656-8bdd0c0a33df").as_deref(),
            Some("claude://code/continue?session=01a072de-0887-7620-a656-8bdd0c0a33df")
        );
    }

    #[test]
    fn a_chat_conversation_opens_the_chat_url_not_code() {
        assert_eq!(
            url_for(Harness::CLAUDE_CHAT, "00000000-1111-2222-3333-444444444444").as_deref(),
            Some("claude://claude.ai/chat/00000000-1111-2222-3333-444444444444")
        );
        assert_eq!(
            url_for(Harness::CLAUDE_CODE, "00000000-1111-2222-3333-444444444444").as_deref(),
            Some("claude://code/continue?session=00000000-1111-2222-3333-444444444444")
        );
    }

    #[test]
    fn a_user_shaped_or_attach_id_is_not_turned_into_a_url() {
        assert_eq!(url_for(Harness::CODEX, "../etc/passwd"), None);
        assert_eq!(url_for(Harness::CODEX, "codex://threads/x"), None);
        assert_eq!(
            url_for(Harness::CODEX, "warm:01a06307-1111-2222-3333-444444444444"),
            None
        );
        assert_eq!(url_for(Harness::CODEX, "thr_bus_owned"), None);
        assert_eq!(
            url_for(Harness::CURSOR, "01a06307-1111-2222-3333-444444444444"),
            None
        );
        assert_eq!(url_for(Harness::CODEX, "short"), None);
    }

    #[test]
    fn reveal_hands_the_official_url_to_open() {
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Mutex;
        static ENV: Mutex<()> = Mutex::new(());
        let dir = tempfile::tempdir().expect("tempdir");
        let stub = dir.path().join("open");
        let mark = dir.path().join("url");
        std::fs::write(
            &stub,
            format!(
                "#!/bin/sh\nfor a in \"$@\"; do last=\"$a\"; done\nprintf '%s' \"$last\" > {}\n",
                mark.display()
            ),
        )
        .expect("stub");
        std::fs::set_permissions(&stub, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let _guard = ENV.lock().unwrap_or_else(|e| e.into_inner());
        unsafe {
            std::env::set_var("CONBUS_OPEN_BIN", &stub);
        }
        let opened = reveal(Harness::CODEX, "01a06307-1111-2222-3333-444444444444");
        let got = std::fs::read_to_string(&mark);
        unsafe {
            std::env::remove_var("CONBUS_OPEN_BIN");
        }
        assert!(opened.expect("open"));
        assert_eq!(
            got.expect("url"),
            "codex://threads/01a06307-1111-2222-3333-444444444444"
        );
    }
}
