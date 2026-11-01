# Cursor hook contract — captured 2026-09-06

Cursor invokes `conbus hook` two ways, and both are live. Its own
`~/.cursor/hooks.json` carries the five lowercase events `conbus install`
registers there; the tool events below still arrive through Claude Code's
settings file, which Cursor also reads. The payload is not Claude's either
way. Identification is the `cursor_version` field. Field names
below were pinned by the live drop log (`no mapping for hook event "…"`,
names only) and match Cursor's own hook docs on types.

## Events seen live (with `cursor_version`)

| Event | Role |
|---|---|
| `sessionStart` | session candidate; no `source` |
| `beforeSubmitPrompt` | turn start; `prompt` |
| `preToolUse` | tool request; `tool_name`, `tool_input`, `tool_use_id` |
| `postToolUse` | tool result; `tool_output`, `duration` (ms) |
| `stop` | turn end; `status` is `completed` \| `aborted` \| `error` |
| `subagentStop` | subagent turn end; `subagent_id` |

## Identity

- `conversation_id` is the stable conversation. `session_id` is documented as
  the same value; both appear on live events.
- `generation_id` changes every user message — the turn correlator.
- `workspace_roots` is a string array. The first root is the checkout.
- `user_email` arrives and is never stored.

## Reply

`{"continue":true,"suppressOutput":true}` has been accepted thousands of times
on this machine (the hook was failing the *mapping*, not the reply). Cursor's
`beforeSubmitPrompt` does not take Claude's `hookSpecificOutput`. Official
inject is `sessionStart` → `additional_context`. The bus briefs that event
and records `beforeSubmitPrompt` so the conversation can mint.

## cursor-agent 2026.10.19 — print mode

`cursor-agent -p` fires `sessionStart`, `preToolUse`, `beforeShellExecution`,
`afterShellExecution`, `postToolUse`, `sessionEnd` only — no
`beforeSubmitPrompt`, `afterAgentResponse`, or `stop`. It ignores a
project's `.claude/settings.json`; hooks come from its own registration.
