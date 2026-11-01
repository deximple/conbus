# OpenCode 2.2.3 plugin contract — captured 2026-11-01

`opencode_2.2.3_events.jsonl` is what a 2.x plugin's `ctx.event.subscribe()`
yielded during one `opencode run` with a fixed prompt and one shell call, in a
throwaway checkout. Paths are `/Users/USER/repo` and ids are replaced; the
provider, model, plugin and instruction events of the same run are left out.

## Loading

- 2.x loads a plugin module as **one default export `{ id, setup(ctx) }`**
  (or `effect`). It refuses the 1.x module — named function exports — with
  `Plugin must export a default definition with an id and an effect or setup
  function`, and a function default export carrying `id` and `setup` as
  properties is refused the same way. The refusal is in OpenCode's own log
  only; nothing else says the plugin is not running.
- `server()` beside `setup()` was never called.
- Project plugins load from `<project>/.opencode/plugins/`, user plugins from
  `~/.config/opencode/plugins/`, and edits are hot-reloaded.

## Events conbus uses

- `session.inbox.enqueued`: `data.sessionID`, `data.inboxID`, and
  `data.item` `{ type: "user", payload: { text, files }, delivery }` — a
  person's prompt, sent to the hook once per `inboxID` as the payload the 1.x
  plugin sent for a prompt.
- `session.execution.failed`: sent as `session.error` with `data.error`. Not
  captured live (no failure in this run); its shape is unverified.
- `location.directory` on most events is the checkout.

No `session.created` is published on the bus; a conversation starts at its
first prompt. Turn ends (`session.execution.succeeded`), tool calls
(`session.tool.*`) and tokens (`session.usage.updated`, `session.step.ended`)
are published and not yet forwarded.
