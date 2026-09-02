# Codex 0.152.1 hook contract — from traffic, not from the schema

`codex_0.152.1.jsonl` is real traffic captured on 2026-09-03 with a tee hook in a
throwaway checkout: one interactive session (two turns, one `ls -la`) plus the
session start and prompt of the memory-writing agent Codex spawned beside it.
Username redacted to `user`; the agent's 50 KB instruction prompt cut to its
first line. Everything the 0.147.0 contract inferred from the embedded schemas
held, with the additions and surprises below. Codex's own reference for the
output side: <https://developers.openai.com/codex/hooks> (redirects to
learn.chatgpt.com/docs/hooks).

## Input — what every payload carries

- Common base: `session_id`, `transcript_path` (string **or null**), `cwd`,
  `hook_event_name`, `model`, `permission_mode` (`bypassPermissions` under
  YOLO mode; absent on `SessionEnd`), `turn_id` on turn-scoped events.
- `SessionStart`: `source` — `startup` observed; `resume | clear | compact` per
  the reference. **Fires lazily at the first prompt, not when the TUI opens** —
  a session that is opened and closed without a prompt fires `SessionEnd` only.
  Those promptless ends reach the daemon as orphans (no binding), which is one
  of the shapes behind the orphan drops in the drop log.
- `UserPromptSubmit`: `prompt`.
- `PreToolUse` / `PostToolUse`: `tool_name` (`Bash`), `tool_use_id` in the form
  `exec-<uuid>`, `tool_input` (`{"command": …}`), and on `PostToolUse` a
  `tool_response` **string** (the command's output, 17 KB in one capture) —
  not an object as the synthetic tests assumed.
- `Stop`: `stop_hook_active`, `last_assistant_message`.
- `SessionEnd`: `reason` — still the single constant `other`.

## The memory-writing agent

Codex 0.152.1 spawns a **"Memory Writing Agent"** beside an interactive session
once it has something to consolidate: its own `session_id`,
`transcript_path: null`, `cwd` = `~/.codex/memories`, `model` = a different
model (`gpt-5.6-terra` beside `gpt-5.6-sol`), a `UserPromptSubmit` whose
prompt begins `## Memory Writing Agent: Phase 2 (Consolidation)`, several
`PreToolUse`/`PostToolUse` pairs reading and writing memory files, `SessionEnd`
— and **no `Stop`**. It is a machine's conversation delivered through the
person's hooks. The recorder mints it as a conversation of its own in the
memory workspace; nothing marks it as machine-authored yet (the prompt prefix
is the honest tell, the same way `<scheduled-task` is for Claude Code's
scheduled runs).

## Output — what a hook may say

Per the reference, and confirmed the hard way:

- `PreToolUse` accepts only `systemMessage` and the `hookSpecificOutput`
  permission decisions. **`continue`, `stopReason` and `suppressOutput` fail
  the hook.** The TUI shows "PreToolUse hook (failed) — error: PreToolUse hook
  returned unsupported suppressOutput" on every tool call; the record had
  already been sent, so the recorder was right and the person saw it failing.
- `PostToolUse` accepts `systemMessage`, `continue: false`, `stopReason`;
  `suppressOutput` is "parsed but unsupported" — the TUI reports it as a
  failure too.
- `SessionStart`, `UserPromptSubmit`, `Stop` (and the compact events) accept
  the common fields plus `hookSpecificOutput.additionalContext`.
- `SessionEnd` output is advisory only.
- The empty object `{}` is accepted by every event. That is what
  `silent_reply` says to Codex now; Claude Code keeps
  `{"continue":true,"suppressOutput":true}` because a zero-byte stdout once
  took a real session down there.
- A briefing works: `{"systemMessage": …, "hookSpecificOutput":
  {"hookEventName": "UserPromptSubmit", "additionalContext": …}}` on
  `UserPromptSubmit` reached the model — Codex printed the `systemMessage`
  ("UserPromptSubmit (completed) says: conbus · 1 earlier session in this
  checkout …") and "hook context: <conbus-record …>", and the model quoted the
  record's first line and counted its two quote blocks. `hookEventName` is
  tolerated inside `hookSpecificOutput`.
- Model-visible hook output is capped at ~2,500 tokens (`additionalContextLimit`,
  0 lifts it); the excess spills to `<temp_dir>/hook_outputs/<session_id>/…`
  with a preview sent to the model. The pack budget (2,600 UTF-16 units) is
  well under it.

## Trust, and how it was granted

- Hooks do not run until trusted. The gate is the CLI's startup dialog:
  **"Hooks need review — N hooks are new or changed. 1. Review hooks 2. Trust
  all and continue 3. Continue without trusting"**. It reviews every untrusted
  hook across every file Codex loaded (here 19: conbus's six, paseo's five, a
  test checkout's six, two bundled plugin hooks), so "trust all" is one gesture
  for all of them. The record lands as `[hooks.state."<file>:<event>:<group>:<index>"]`
  in `config.toml`; conbus reads it and never writes it.
- Project-local `.codex/hooks.json` loads only after the **directory** is
  trusted ("Do you trust the contents of this directory?"), recorded under
  `[projects."<path>"]`.
- A `SessionEnd` fired before trust was granted — the hook ran; the trust
  gate skips untrusted hooks per hook, not per session.
- Async hooks (`"async": true`) are allowed from 0.148 (0.147.0 dropped them),
  max 8 per session, never for `SessionEnd`. conbus registers none.
- `codex exec` (non-interactive) hung at MCP start-up on this machine (four
  Cloudflare servers wanting OAuth, one failing to start) and produced no
  session; the interactive path is what these fixtures come from. Unverified
  whether `exec` fires hooks at all.
- The TUI enables bracketed paste: text and Enter written in one burst are
  taken as a paste, and the prompt is not submitted. Drive it with the text
  first and the Enter separately.

## Quota probe #1 — `account/rateLimits/read` (2026-09-03 03:30 KST)

The plan's first probe: does the app-server expose the account's rate limits
to a client that is not a Codex session? It does, on the first try, read-only,
with `initialize` (`capabilities.experimentalApi: true`) then
`account/rateLimits/read` with empty params:

```json
{"rateLimits": {"limitId": "codex", "limitName": null,
  "primary":   {"usedPercent": 22, "windowDurationMins": 300,   "resetsAt": 1788385869},
  "secondary": {"usedPercent": 9,  "windowDurationMins": 10080, "resetsAt": 1788752527},
  "credits": {"hasCredits": false, "unlimited": false, "balance": "0"},
  "individualLimit": null, "spendControlReached": false, "planType": "plus",
  "rateLimitReachedType": null},
 "rateLimitsByLimitId": {"codex": {…same…}},
 "rateLimitResetCredits": {"availableCount": 1, "credits": [{"resetType": "codexRateLimits",
  "status": "available", "title": "Full reset (Weekly + 5 hr)", "grantedAt": …, "expiresAt": …}]},
 "accountId": "<uuid>", "rateLimitUpsell": null}
```

So: account-scoped (`accountId`), a five-hour window and a weekly window each
with `usedPercent` and an absolute `resetsAt`, `rateLimitReachedType` as the
"exhausted" signal (`null` here), and the reset credits the June PR added. This
is the signal the router was designed around, and it is readable without
opening a thread. Not yet wired into conbus; the obvious next brick is a
`conbus quota` command that runs this probe and a `doctor` line that shows it.
