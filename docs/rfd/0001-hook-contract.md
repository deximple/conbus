# RFD 0001 — Claude-settings-compatible hook contract

Status: published for the public launch path.
First public artefact. Bus id / event schema RFDs follow this one.

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
| Grok | `~/.grok/hooks/conbus.json` | 9 | `{bin} hook --harness grok` |
| Cursor | `~/.cursor/hooks.json` | 5 | `{bin} hook --harness cursor` |
| OpenCode | `~/.config/opencode/plugins/conbus.js` | plugin bus | `{bin} hook --harness opencode` |

Claude Code also wraps StatusLine: `{bin} hook --statusline`. The person's
original status bar command is saved at `~/.claude/conbus-statusline-forward`.

Codex: never `"async": true` (0.147.0 drops those hooks). Never write
`trusted_hash`. Trust is a gesture inside Codex.

Cursor dialect is not Claude's. Official inject is `additional_context`
on `sessionStart`. `beforeSubmitPrompt` has `continue` and `user_message`.

## Events we register (Claude Code)

`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`,
`PostToolUseFailure`, `PostToolBatch`, `PermissionRequest`,
`PermissionDenied`, `Notification`, `Stop`, `StopFailure`,
`SubagentStop`, `SessionEnd`.

`bus_id` is minted on the first `UserPromptSubmit`, never on
`SessionStart`. Cursor also mints on a completed turn when that prompt
was never seen.

## Reply dialect

A hook must exit zero. A non-zero `UserPromptSubmit` blocks the prompt.

- Claude Code: `{"continue":true,"suppressOutput":true}` when silent.
  Briefing uses `hookSpecificOutput.additionalContext`.
- Codex: `{}` on every event. `continue` / `suppressOutput` fail the hook
  on 0.152.1.
- Cursor `sessionStart`: `additional_context` (snake_case).

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
