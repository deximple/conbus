// Written by conbus install. Forwards official plugin events; does not call vendor HTTP.
//
// One prompt is one `turn_started`, carrying the person's own words. OpenCode
// (read in its source at 1.18.31) publishes a person's message before its
// parts, and publishes it again after every agent step, because its summariser
// writes the step's diff onto the message (anomalyco/opencode#43551). So
// `message.updated` only says whose a message is. The prompt is the first text
// part of a person's message that is neither `synthetic` (OpenCode wrote it)
// nor `ignored` — the part OpenCode's own timeline shows as what was said — and
// it is sent once per message id. The agent's reply arrives as text parts too,
// so a part is sent only once its message is known to be the person's; a part
// that comes before its message waits for it. OpenCode calls `event` without
// awaiting it, so each call decides everything before its first await.
import { spawn } from "node:child_process";

// How many message ids keep their role, and how many early parts may wait.
// Both forget their oldest entry first, so a long session cannot grow them.
const KNOWN = 1024;
const WAITING = 64;

export const ConbusPlugin = async ({ directory }) => {
  const bin = "@CONBUS_BIN@";
  const send = (payload) =>
    new Promise((resolve) => {
      try {
        const child = spawn(bin, ["hook", "--harness", "opencode"], {
          stdio: ["pipe", "pipe", "pipe"],
        });
        child.on("error", () => resolve());
        child.on("close", () => resolve());
        // A hook that exits before reading fails this write with EPIPE, and
        // an unheard stream error is an uncaught exception in OpenCode itself.
        child.stdin.on("error", () => {});
        child.stdin.end(JSON.stringify(payload));
      } catch {
        resolve();
      }
    });

  // message id -> "user", "sent" once its prompt has gone, or "other".
  const known = new Map();
  // message id -> the prompt of a text part that came before its message.
  const waiting = new Map();
  const remember = (map, key, value, limit) => {
    map.delete(key);
    map.set(key, value);
    if (map.size > limit) map.delete(map.keys().next().value);
  };

  const learn = async (info) => {
    const id = info?.id;
    if (!id) return;
    const early = waiting.get(id);
    waiting.delete(id);
    if (info.role !== "user") {
      remember(known, id, "other", KNOWN);
      return;
    }
    // The summariser publishing a sent message again changes nothing.
    if (known.get(id) === "sent") {
      remember(known, id, "sent", KNOWN);
      return;
    }
    remember(known, id, early ? "sent" : "user", KNOWN);
    if (early) await send(early);
  };

  const offer = async (part, session) => {
    if (part?.type !== "text" || part.synthetic || part.ignored || !part.text) return;
    const prompt = {
      opencode_plugin: 1,
      hook_event_name: "message.part.updated",
      session_id: part.sessionID ?? session,
      cwd: directory,
      message_id: part.messageID,
      prompt: part.text,
    };
    const id = prompt.message_id;
    if (!id || !prompt.session_id) return;
    const role = known.get(id);
    if (role === "user") {
      remember(known, id, "sent", KNOWN);
      await send(prompt);
    } else if (role === undefined && !waiting.has(id)) {
      remember(waiting, id, prompt, WAITING);
    }
  };

  return {
    event: async ({ event }) => {
      const type = event?.type;
      const props = event?.properties ?? {};
      if (type === "message.updated") return learn(props.info);
      if (type === "message.part.updated") return offer(props.part, props.sessionID);
      const session = props.sessionID ?? props.info?.sessionID ?? props.info?.id;
      if (!session) return;
      if (type === "session.created") {
        await send({
          opencode_plugin: 1,
          hook_event_name: type,
          session_id: session,
          cwd: directory,
        });
      } else if (type === "session.error") {
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
