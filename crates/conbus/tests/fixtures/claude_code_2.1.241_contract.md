# Claude Code hook contract — 2.1.241

Extracted from the schema definitions embedded in the shipped binary on 2026-08-30.
This is the contract, not a guess. `strings` alone got `StopFailure` wrong: `failure_reason`
is present in the binary but is **not** the payload field — the field is `error`.

Every payload also carries the common base (`v_`, one builder, spread at every emit site):

    session_id: string
    transcript_path: string
    cwd: string
    prompt_id: string?      # "UUID correlating a user prompt with all subsequent events
                            #  until the next prompt. Same value emitted on OpenTelemetry
                            #  events as the `prompt.id` attribute... Absent until the first
                            #  user input of the process lifetime."
    permission_mode: string?
    agent_id: string?       # "Subagent identifier. Present only when the hook fires from
                            #  within a subagent... Absent for the main thread, even in
                            #  --agent sessions. Use this field (not agent_type) to
                            #  distinguish subagent calls from main-thread calls."
    agent_type: string?     # Agent type name. With agent_id inside a subagent, or on the
                            #  main thread of a session started with --agent.
    effort: {level}?        # Reasoning effort. Tool-context hooks only; absent on
                            #  session-lifecycle hooks. Observed: {"level":"high"}.

`agent_id` is the one that changes a design. A subagent's `PostToolUse` arrives under the
**parent's** `session_id` — the agent identity travels only in `agent_id`. Mirroring without it
interleaves two agents' work under one conversation with nothing on the line to separate them.
`prompt_id` does not help: it is one slot on a process-global request journal, identical for a
subagent and its parent.

### `prompt_id` is process-global, so exactly one event can carry another conversation's turn

It is one slot on a request journal, rotated at prompt submit. That makes it safe everywhere the
vendor set it for the session in hand, and unsafe in one place:

- `SessionStart` on **resume** or **fork** — the hook runs before the reset, so the value is the
  *pre-resume conversation's*. This is the only cross-conversation case, and the only one the bus
  drops.
- `SessionStart` on `startup` and `clear`: none at all (`clear` resets first; both captures confirm
  startup carries none).
- `SessionStart` on **compact**: the turn the compaction happened inside — live, and kept.
- `SessionEnd`: the turn that just finished (confirmed in both captures) — this conversation's, by
  the field's own definition of "all subsequent events until the next prompt". Kept.

An unfamiliar `SessionStart` source is treated as unsafe. A missing correlator costs a join; a
wrong one corrupts the conversation graph.

### `MessageDisplay.turn_id` is NOT `prompt_id`

Both appear on the same payload with different UUIDs — verified in
`claude_code_2.1.241_probe2.jsonl`. The vendor's `turn_id` is a different concept from the turn
correlator. That is why the bus records `prompt_id` under the name `vendor_turn_id`: naming ours
`turn_id` would claim the vendor's word for another meaning, and would collide with it the moment
`MessageDisplay` is mirrored, because `Record` flattens the event body and a duplicate key makes
the line unreadable on scan.

### `UserPromptSubmit.source` exists in the schema and is never sent

    source: enum["user","sdk","system","loop_wakeup","schedule_wakeup","poll_event"]?
            "...Payloads may omit it while the field rolls out."

**And the transcript's vocabulary is larger than the hook schema's.** Backfilling 3,425 real sessions
turned up `typed`, `queued` and `suggestion_accepted` in the transcript's `promptSource` field —
none of which are among the six the hook schema declares. Whatever the relationship between the two
fields, it is not identity, and it is the reason `Author` is an open string rather than an enum.

Both emit sites have the spread constant-folded out in 2.1.241. Confirmed empirically: a `claude -p`
run, which must be `sdk`, produced a `UserPromptSubmit` with no `source` key. Parse it if it
appears; never gate behaviour on it, and never read its absence as `user`.

`StopFailure.error` is one of eleven values:

    authentication_failed  oauth_org_not_allowed  account_on_hold  billing_error
    rate_limit             overloaded             invalid_request  model_not_found
    server_error           unknown                max_output_tokens

`credits_required` is real but is an API-level 429 `error_code`; it surfaces here as
`error: "rate_limit"`. It is not a `StopFailure` value.

`SessionStart.source` is one of five: startup, resume, clear, compact, fork.
Subagents are **not** a session source — they get their own `SubagentStart` / `SubagentStop`. But
they are also not a separate stream: their tool calls land on the ordinary `PostToolUse` stream
under the parent `session_id`, told apart only by `agent_id`.

`SessionEnd.reason` is one of five: clear, resume, logout, prompt_input_exit, other.

`PermissionDenied` fires from **one** call site, guarded by
`decisionReason.type === "classifier" && decisionReason.classifier === "auto-mode"`. It is not
emitted when a person simply answers no at the prompt, so it does not make every tool request
answerable. `PermissionRequest` — the pre-decision event, which carries no `tool_use_id` — is where
an ordinary refusal is visible. conbus records it as `permission_asked`, not as a denial.

## Payloads

### PreToolUse

    tool_name:string,tool_input:json,tool_use_id:string

### PermissionRequest

    tool_name:string,tool_input:json,permission_suggestions:array(DFn())?

### PostToolUse

    tool_name:string,tool_input:json,tool_response:json,tool_use_id:string,duration_ms:number?

### PostToolUseFailure

    tool_name:string,tool_input:json,tool_use_id:string,error:string,is_interrupt:bool?,duration_ms:number?

### PostToolBatch

    tool_calls: array of {
        tool_name: string
        tool_input: json
        tool_use_id: string
        tool_response: json?      # absent when the call did not return one
    }

    "Fired once after every tool call in a batch has resolved, before the next model
     request. PostToolUse fires per-tool and may run concurrently for parallel tool
     calls; PostToolBatch fires exactly once with the full batch."

    conbus records only the ids. The ids are the only part the per-call events do not
    already carry: under `Full` retention the bodies would be written a third time, and
    under the shipping default they would be elided to a byte count a third time. Either
    way the batch line is the grouping and the boundary, nothing else.

### PermissionDenied

    tool_name:string,tool_input:json,tool_use_id:string,reason:string

### Notification

    message:string,title:string?,notification_type:string

### UserPromptSubmit

    prompt:string,source:enum["user","sdk","system","loop_wakeup","schedule_wakeup","poll_event"])?,session_title:string?

### UserPromptExpansion

    expansion_type:enum["slash_command","mcp_prompt"]),command_name:string,command_args:string,command_source:string?,prompt:string

### SessionStart

    source:enum["startup","resume","clear","compact","fork"]),agent_type:string?,model:string?,session_title:string?

### Setup

    trigger:enum["init","maintenance"])

### Stop

    stop_hook_active:bool,last_assistant_message:string?,background_tasks:array(wBd())?,session_crons:array(EBd())?

### StopFailure

    error:Iha(),error_details:string?,last_assistant_message:string?

### SubagentStart

    agent_id:string,agent_type:string

### SubagentStop

    stop_hook_active:bool,agent_id:string,agent_transcript_path:string,agent_type:string,last_assistant_message:string?,background_tasks:array(wBd())?,session_crons:array(EBd())?

### PreCompact

    trigger:enum["manual","auto"]),custom_instructions:string|null

### PostCompact

    trigger:enum["manual","auto"]),compact_summary:string

### TeammateIdle

    teammate_name:string,team_name:string

### TaskCreated

    task_id:string,task_subject:string,task_description:string?,teammate_name:string?,team_name:string?

### TaskCompleted

    task_id:string,task_subject:string,task_description:string?,teammate_name:string?,team_name:string?

### Elicitation

    mcp_server_name: string, message: string, mode: enum["form","url"]?, url: string?,
    elicitation_id: string?, requested_schema: map(string, json)?

### ElicitationResult

    mcp_server_name: string, elicitation_id: string?, mode: enum["form","url"]?,
    action: enum["accept","decline","cancel"], content: map(string, json)?

### ConfigChange

    source: enum["user_settings","project_settings","local_settings","policy_settings","skills"], file_path: string?

### InstructionsLoaded

    file_path:string,memory_type:Dr(n6b),load_reason:Dr(r6b),globs:array(string)?,trigger_file_path:string?,parent_file_path:string?

### WorktreeCreate

    name:string

### WorktreeRemove

    worktree_path:string

### CwdChanged

    old_cwd:string,new_cwd:string

### FileChanged

    file_path:string,event:enum["change","add","unlink"])

### DirectoryAdded

    directory:string,source:enum["slash_command","register_repo_root"])

### MessageDisplay

    turn_id: string, message_id: string, index: number, final: bool, delta: string

    `turn_id` here is NOT the turn correlator conbus records: the same payload carries
    both `prompt_id` and `turn_id` with different UUIDs. See the note above.

    `delta` is "the newly completed lines since the prior flush", so this fires many
    times per assistant message. conbus does not mirror it: it is the only official
    channel carrying every assistant message rather than just the last, and it collides
    with the shipping default of writing conversation but not tool bodies. A retention
    decision before it is an adapter change.

### SessionEnd

    reason:p6b()
