# Conbus

A conversation bus for coding agents. One conversation keeps its identity while the harness underneath it changes.

Design lives in `.cursor/plans/shared_workspace_context_3f7139b8.plan.md` and is the source of truth. Session state and the next task are in `HANDOFF.md`. Read both before proposing architecture; four review rounds already closed most of the open questions, and reopening them wastes turns. The Claude Code hook contract and the real payloads that pin it are in `crates/conbus/tests/fixtures/` — read those before changing an adapter, because field names taken from anything else have already been wrong once.

## Non-negotiable constraints

These are legal or structural, not preferences.

- Never read, store, or intermediate vendor auth tokens. Login happens only through the unmodified vendor binary's own flow.
- Never modify a vendor binary or strip its authentication.
- Never pool credentials or quota across people.
- A person's own accounts on one vendor may each be a profile, through the vendor's own documented mechanism (Claude Code's `CLAUDE_CONFIG_DIR`), each logged in by the vendor's own flow. Switching between profiles is manual only: no limit signal, router rule or hop ever moves a conversation from one of a person's accounts to another. The vendor's terms forbid circumventing its protective measures, and a usage limit is one.
- Never parse vendor session files on the live path. Mirroring, gap filling, and routing use official channels. Backfill is the only exception, and only through a fixture-protected parser that skips unknown schemas.
- Never write into a Codex thread the user opened (`thread/inject_items`). Our warm thread is one we opened through app-server. The hook's `additionalContext` on `UserPromptSubmit` is the vendor's own context channel for Claude Code and Codex. Cursor's official inject is `additional_context` on `sessionStart`.
- Never expose a secret-reading tool to the model. Secrets are injected as child env at spawn.
- Never use vendor trademarks in product or feature names.
- Never send the rolling summary to the leg it is protecting.

## Decided, do not redesign

- Vendor TUIs stay the window. We do not build a chat UI.
- One binary, argv dispatch: `conbus daemon|hook|log|install|backfill|doctor|quota|hop|go|status|run|arena|summary|verdict|sync|usage|progress|policy|--version`. Cargo workspace internally. `hop --apply` opens or syncs a bus-owned warm on both legs (`warm:` Codex, `warm-claude:` Claude). Cursor landing is official `cursor -n <workspace>`. A chat line that *is* an order (`이제 커서로 작업할거야`) is `--to` after the hook reply. It never injects into a thread the user opened. `conbus summary` is the only model-backed digest, and it never uses the protected leg.
- JSONL segments are the source of truth. SQLite is a disposable derived index, always rebuildable from segments. The `cache/` sidecars are derived the same way — delete them and nothing is lost. The `spool/` holds hooks in transit across a restart — drained by the next daemon, deleted after ingest, never synced.
- Data in `~/.conbus/`, respecting `$XDG_STATE_HOME`.
- Derived state lives in the daemon's private memory, not a file-backed mmap ring.
- `bus_id` is minted on the first `UserPromptSubmit`, never on `SessionStart`. Cursor also mints on a completed turn when that prompt was never seen, so a live Cursor conversation can still grow a pack.
- Every log record carries `schema_version` and a vendor/version stamp.
- macOS and Linux are both targets. Linux runs the recorder as a systemd user unit. Where there is no OS key store, tool bodies are elided, not sealed with a key kept in a file.
- Policy is local and off by default. `~/.conbus/policy.json` turns into `PreToolUse` decisions through each vendor's documented hook output, evaluated in the hook process so a down recorder cannot lift a deny. A policy file that does not parse decides nothing, and `doctor` says so. No rule ever comes from a server.
- A static landing page and a waitlist may be hosted. The bus, the log and the usage rows may not, until the money gate in `docs/saas-strategy.md` passes.

## Working style

State assumptions before implementing, and say so when a simpler approach exists. Keep changes surgical: touch only what the task requires, and match the surrounding style. Verify with evidence rather than declaring success — for this project that usually means a real vendor session, because hook failures here are silent by default.

Self-improve is `docs/self-improve.md`. Observe the live bus, pick one live defect, fix it, verify, ledger, repeat. Do not treat a stale HANDOFF paragraph as current.

## Linux and cloud VMs

conbus builds and records on Linux; the recorder runs as a systemd user unit there and `install --apply` expects launchd only on macOS. Set `CONBUS_HOME` when the log should not land in `~/.conbus`, and `CONBUS_SKIP_LAUNCHCTL=1` in CI so install paths never touch a service manager. `conbus doctor` may show `memory_locked=false` on a VM that refuses `mlockall` — the daemon still records.




