# Codex hook contract — codex-cli 0.147.0

Extracted from the draft-07 JSON Schemas embedded in the shipped native binary on 2026-08-31
(`<npm prefix>/@openai/codex-darwin-arm64/vendor/aarch64-apple-darwin/bin/codex`, searched for
`<event>.command.input{`). The contract, not a guess.

## What differs from Claude Code, and matters

- **The correlator is `turn_id`, not `prompt_id`.** Required on nine events, absent on `SessionStart`
  and `SessionEnd` — so unlike Claude's, it is never stale and needs no drop rule.
- **Hooks load from `$CODEX_HOME/hooks.json` and `<project>/.codex/hooks.json`**, NOT `settings.json`.
  `settings.json` appears only in Codex's one-shot `/import` of Claude sessions.
- **`SessionStart.source` has four values** — no `fork`.
- **`SessionEnd.reason` is a single constant**, `other`.
- **`UserPromptSubmit` has no `source`**, so a prompt's author is unknown on every Codex turn.
- **`PostToolUse` has no `duration_ms`.**
- **There is no `PostToolUseFailure`, no `StopFailure`, no `PostToolBatch`, no `PermissionDenied`.**
  A failed tool and a quota refusal do not reach a hook at all; quota has to come from the
  app-server's `account/rateLimits`, which is the plan's probe #1.
- **`PermissionRequest` is a pre-decision hook**, fired on every approval prompt. It is not a record
  of refusal, and mapping it to a denial would log approvals as denials. conbus records it as
  `permission_asked`.
- `model` and `permission_mode` are required on nearly every event; `transcript_path` is nullable.

## Payloads

### PostToolUse

    agent_id?: string
    agent_type?: string
    cwd: string
    model: string
    permission_mode: enum["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]
    session_id: string
    tool_input: any
    tool_name: string
    tool_response: any
    tool_use_id: string
    transcript_path: NullableString
    turn_id: string

### PermissionRequest

    agent_id?: string
    agent_type?: string
    cwd: string
    model: string
    permission_mode: enum["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]
    session_id: string
    tool_input: any
    tool_name: string
    transcript_path: NullableString
    turn_id: string

### PostCompact

    agent_id?: string
    agent_type?: string
    cwd: string
    model: string
    session_id: string
    transcript_path: NullableString
    trigger: enum["manual", "auto"]
    turn_id: string

### PreCompact

    agent_id?: string
    agent_type?: string
    cwd: string
    model: string
    session_id: string
    transcript_path: NullableString
    trigger: enum["manual", "auto"]
    turn_id: string

### PreToolUse

    agent_id?: string
    agent_type?: string
    cwd: string
    model: string
    permission_mode: enum["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]
    session_id: string
    tool_input: any
    tool_name: string
    tool_use_id: string
    transcript_path: NullableString
    turn_id: string

### SessionEnd

    cwd: string
    reason: const "other"
    session_id: string
    transcript_path: NullableString

### SessionStart

    cwd: string
    model: string
    permission_mode: enum["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]
    session_id: string
    source: enum["startup", "resume", "clear", "compact"]
    transcript_path: NullableString

### Stop

    cwd: string
    last_assistant_message: NullableString
    model: string
    permission_mode: enum["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]
    session_id: string
    stop_hook_active: boolean
    transcript_path: NullableString
    turn_id: string

### SubagentStart

    agent_id: string
    agent_type: string
    cwd: string
    model: string
    permission_mode: enum["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]
    session_id: string
    transcript_path: NullableString
    turn_id: string

### SubagentStop

    agent_id: string
    agent_transcript_path: NullableString
    agent_type: string
    cwd: string
    last_assistant_message: NullableString
    model: string
    permission_mode: enum["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]
    session_id: string
    stop_hook_active: boolean
    transcript_path: NullableString
    turn_id: string

### UserPromptSubmit

    agent_id?: string
    agent_type?: string
    cwd: string
    model: string
    permission_mode: enum["default", "acceptEdits", "plan", "dontAsk", "bypassPermissions"]
    prompt: string
    session_id: string
    transcript_path: NullableString
    turn_id: string
