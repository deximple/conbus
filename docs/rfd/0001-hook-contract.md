# RFD 0001 — Claude-settings-compatible hook contract

Status: **accepted** for 1.0, 2026-11-01. Part of the 1.x promise
([stability](../stability.md)): a change to the registrations or replies
below is an addition, or it waits for 2.0. First public artefact. Bus id /
event schema RFDs follow this one.

The recorder is one binary. Vendors that already read Claude Code's
`settings.json` hook shape can register `conbus hook` and stop. Field
names come from `crates/conbus/tests/fixtures/`, not from memory.

## Registration

`conbus install --apply` merges, never overwrites. The command is an
absolute path because a vendor `PATH` is not ours.

| Harness | File | Events | Command |
|---|---|---|---|
| Claude Code | `~/.claude/settings.json` | 13 | `{bin} hook` |
| Codex | `$CODEX_HOME/hooks.json` | 7 | `{bin} hook --harness codex` |
| Grok | `~/.grok/hooks/conbus.json` | 10 | `{bin} hook --harness grok` |
| Cursor | `~/.cursor/hooks.json` | 5 | `{bin} hook --harness cursor` |
| OpenCode | `~/.config/opencode/plugins/conbus.js` | plugin bus (1.x or 2.x module, chosen by `opencode --version`) | `{bin} hook --harness opencode` |

Claude Code also wraps StatusLine: `{bin} hook --statusline`. The person's
original status bar command is saved at `~/.claude/conbus-statusline-forward`.

Codex: never `"async": true` (0.147.0 drops those hooks; 0.148 allows
them again, and conbus registers none either way). Never write
`trusted_hash`. Trust is a gesture inside Codex.

Cursor's payload is not Claude's — identification is `cursor_version`, and
the prompt field is `prompt`, not Claude's `prompt` plus envelope. Official
inject is `additional_context` on `sessionStart`. Field names come from
`tests/fixtures/cursor_hooks_contract.md`, which is pinned by live traffic.

Grok's hook docs say it reads Claude Code's `.claude/settings.json` and
Cursor's `.cursor/hooks.json` beside its own hooks directory, and it runs
every registration it finds. With both of ours in place, every Grok event
reaches `conbus hook` twice — once as `--harness grok`, once under Claude
Code's name — and the payload names Grok both times. The hook records the
`--harness grok` copy and answers the other without recording it, but only
while `~/.grok/hooks/conbus.json` registers the recorder for that event
through a binary that exists. Without that file, Claude Code's registration is
the only copy and it is recorded.

## Events we register (Claude Code)

`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`,
`PostToolUseFailure`, `PostToolBatch`, `PermissionRequest`,
`PermissionDenied`, `Notification`, `Stop`, `StopFailure`,
`SubagentStop`, `SessionEnd`.

`bus_id` is minted on the first `UserPromptSubmit`, never on
`SessionStart`. Cursor also mints on work the bus can see it doing — a
completed turn, or a tool call — because its prompt hook lives in its own
hooks file while its tool hooks arrive through Claude Code's, and the two do
not always both fire. A Cursor session that only *ends* still orphans.

## Reply dialect

A hook must exit zero. A non-zero `UserPromptSubmit` blocks the prompt.

- Claude Code: `{"continue":true,"suppressOutput":true}` when silent.
  Briefing uses `hookSpecificOutput.additionalContext`.
- Codex: `{}` on every event. `continue` / `suppressOutput` fail the hook
  on 0.152.1.
- Cursor `sessionStart`: `additional_context` (snake_case). The reply also
  carries Claude's `continue` / `suppressOutput`, which Cursor accepts and
  ignores; only the inject key differs.

Zero-byte stdout on a context-injecting Claude event has taken a session
down (`cache_control` on an empty text block). Always print a JSON object.

## What we never do

- Read, store, or intermediate vendor auth tokens.
- Parse vendor session files on the live path.
- Write into a Codex thread the person opened.
- Send a rolling summary to the leg it is protecting.
- Readdir `~/Downloads` from the daemon.

## Fixtures

Golden payloads live in `crates/conbus/tests/fixtures/`. An adapter
change that invents a field name is already wrong once.
