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

/// Plugin that forwards official bus events to `conbus hook --harness opencode`.
pub fn opencode_plugin_source(binary: &Path) -> String {
    let bin = binary.display();
    format!(
        r#"// Written by conbus install. Forwards official plugin events; does not call vendor HTTP.
import {{ spawn }} from "node:child_process";

export const ConbusPlugin = async ({{ directory }}) => {{
  const bin = {bin:?};
  const send = (payload) =>
    new Promise((resolve) => {{
      const child = spawn(bin, ["hook", "--harness", "opencode"], {{
        stdio: ["pipe", "pipe", "pipe"],
      }});
      child.on("error", () => resolve());
      child.on("close", () => resolve());
      child.stdin.end(JSON.stringify(payload));
    }});

  return {{
    event: async ({{ event }}) => {{
      const type = event?.type;
      const props = event?.properties ?? {{}};
      const session = props.sessionID ?? props.info?.sessionID ?? props.info?.id;
      if (!type || !session) return;
      if (type === "session.created") {{
        await send({{
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
        }});
        return;
      }}
      if (type === "message.updated" && props.info?.role === "user") {{
        await send({{
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
          message_id: props.info?.id,
          prompt: props.info?.summary?.title ?? "",
        }});
        return;
      }}
      if (type === "message.part.updated" && props.part?.type === "text" && props.part?.text) {{
        await send({{
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: props.part.sessionID ?? session,
          cwd: directory,
          message_id: props.part.messageID,
          prompt: props.part.text,
        }});
        return;
      }}
      if (type === "session.error") {{
        await send({{
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
          error: props.error ?? null,
        }});
      }}
    }},
  }};
}};
"#
    )
}

pub fn opencode_plugin_registered(home: &Path) -> bool {
    std::fs::read_to_string(opencode_plugin_path(home))
        .is_ok_and(|text| text.contains("opencode_plugin") && text.contains("session.error"))
}

/// Write the OpenCode plugin. Idempotent when the command path is unchanged.
pub fn apply_opencode_plugin(binary: &Path, home: &Path) -> io::Result<bool> {
    let path = opencode_plugin_path(home);
    let source = opencode_plugin_source(binary);
    if std::fs::read_to_string(&path).is_ok_and(|existing| existing == source) {
        return Ok(false);
    }
    write_atomically(&path, &source)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_plugin_is_written_beside_other_local_plugins() {
        let home = tempfile::tempdir().expect("tempdir");
        let bin = Path::new("/opt/conbus");
        assert!(apply_opencode_plugin(bin, home.path()).expect("write"));
        assert!(!apply_opencode_plugin(bin, home.path()).expect("idempotent"));
        let text = std::fs::read_to_string(opencode_plugin_path(home.path())).expect("read");
        assert!(text.contains("/opt/conbus"));
        assert!(text.contains("session.error"));
        assert!(opencode_plugin_registered(home.path()));
    }
}
