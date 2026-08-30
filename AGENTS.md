# Conbus

A conversation bus for coding agents. One conversation keeps its identity while the harness underneath it changes.

Design lives in `.cursor/plans/shared_workspace_context_3f7139b8.plan.md` and is the source of truth. Session state and the next task are in `HANDOFF.md`. Read both before proposing architecture; four review rounds already closed most of the open questions, and reopening them wastes turns. The Claude Code hook contract and the real payloads that pin it are in `crates/conbus/tests/fixtures/` — read those before changing an adapter, because field names taken from anything else have already been wrong once.

## Non-negotiable constraints

These are legal or structural, not preferences.

- Never read, store, or intermediate vendor auth tokens. Login happens only through the unmodified vendor binary's own flow.
- Never modify a vendor binary or strip its authentication.
- Never pool credentials or quota across people.
- Never parse vendor session files on the live path. Mirroring, gap filling, and routing use official channels. Backfill is the only exception, and only through a fixture-protected parser that skips unknown schemas.
- Never inject into a Codex thread the user opened. Our warm thread is one we opened through app-server.
- Never expose a secret-reading tool to the model. Secrets are injected as child env at spawn.
- Never use vendor trademarks in product or feature names.
- Never send the rolling summary to the leg it is protecting.

## Decided, do not redesign

- Vendor TUIs stay the window. We do not build a chat UI.
- One binary, argv dispatch: `conbus daemon|hook|log|doctor`. Cargo workspace internally.
- JSONL segments are the source of truth. SQLite is a disposable derived index, always rebuildable from segments.
- Data in `~/.conbus/`, respecting `$XDG_STATE_HOME`.
- Derived state lives in the daemon's private memory, not a file-backed mmap ring.
- `bus_id` is minted on the first `UserPromptSubmit`, never on `SessionStart`.
- Every log record carries `schema_version` and a vendor/version stamp.

## Working style

State assumptions before implementing, and say so when a simpler approach exists. Keep changes surgical: touch only what the task requires, and match the surrounding style. Verify with evidence rather than declaring success — for this project that usually means a real vendor session, because hook failures here are silent by default.
