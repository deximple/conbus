// Written by conbus install. Forwards official plugin events; does not call vendor HTTP.
import { spawn } from "node:child_process";

export const ConbusPlugin = async ({ directory }) => {
  const bin = "/Users/you/.local/bin/conbus";
  const send = (payload) =>
    new Promise((resolve) => {
      const child = spawn(bin, ["hook", "--harness", "opencode"], {
        stdio: ["pipe", "pipe", "pipe"],
      });
      child.on("error", () => resolve());
      child.on("close", () => resolve());
      child.stdin.end(JSON.stringify(payload));
    });

  return {
    event: async ({ event }) => {
      const type = event?.type;
      const props = event?.properties ?? {};
      const session = props.sessionID ?? props.info?.sessionID ?? props.info?.id;
      if (!type || !session) return;
      if (type === "session.created") {
        await send({
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
        });
        return;
      }
      if (type === "message.updated" && props.info?.role === "user") {
        await send({
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
          message_id: props.info?.id,
          prompt: props.info?.summary?.title ?? "",
        });
        return;
      }
      if (type === "message.part.updated" && props.part?.type === "text" && props.part?.text) {
        await send({
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: props.part.sessionID ?? session,
          cwd: directory,
          message_id: props.part.messageID,
          prompt: props.part.text,
        });
        return;
      }
      if (type === "session.error") {
        await send({
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
          error: props.error ?? null,
        });
      }
    },
  };
};
