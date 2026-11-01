// Written by conbus install. For OpenCode 2.x; forwards official plugin-bus events; does not call vendor HTTP.
//
// OpenCode 2.x loads a plugin as a default export with an id and a setup
// function, and hands it events through `ctx.event.subscribe()`; it refuses
// the 1.x module this replaces ("Plugin must export a default definition with
// an id and an effect or setup function"). The payloads are the ones the 1.x
// plugin sends, so the recorder reads both the same way. A person's prompt is
// `session.inbox.enqueued` with a `user` item carrying the text they sent, and
// it is sent once per inbox id. A failed execution is the error. 2.x publishes
// no session-created event, so a conversation starts at its first prompt.
import { spawn } from "node:child_process";

// How many inbox ids are remembered; the oldest is forgotten first, so a long
// session cannot grow the set.
const KNOWN = 1024;

export default {
  id: "conbus",
  setup(ctx) {
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

    const sent = new Set();
    const first = (id) => {
      if (sent.has(id)) return false;
      sent.add(id);
      if (sent.size > KNOWN) sent.delete(sent.values().next().value);
      return true;
    };
    const where = (event) => event?.location?.directory ?? ctx?.location?.directory ?? null;

    const controller = new AbortController();
    void (async () => {
      for await (const event of ctx.event.subscribe({ signal: controller.signal })) {
        const data = event?.data ?? {};
        if (event?.type === "session.inbox.enqueued") {
          const item = data.item;
          const text = item?.payload?.text;
          if (item?.type !== "user" || typeof text !== "string" || !text) continue;
          if (!data.sessionID || !data.inboxID || !first(data.inboxID)) continue;
          void send({
            opencode_plugin: 1,
            hook_event_name: "message.part.updated",
            session_id: data.sessionID,
            cwd: where(event),
            message_id: data.inboxID,
            prompt: text,
          });
        } else if (event?.type === "session.execution.failed" && data.sessionID) {
          void send({
            opencode_plugin: 1,
            hook_event_name: "session.error",
            session_id: data.sessionID,
            cwd: where(event),
            error: data.error ?? null,
          });
        }
      }
    })().catch(() => {});
    return () => controller.abort();
  },
};
