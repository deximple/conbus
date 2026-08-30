# Claude Code hook contract — 2.1.241

Extracted from the schema definitions embedded in the shipped binary on 2026-08-30.
This is the contract, not a guess. `strings` alone got `StopFailure` wrong: `failure_reason`
is present in the binary but is **not** the payload field — the field is `error`.

Every payload also carries the common base:

    session_id: string
    transcript_path: string
    cwd: string
    prompt_id: string?     # UUID correlating a user prompt with all subsequent
                           # events until the next prompt. Absent until the first
                           # user input of the process lifetime.
    permission_mode: string?

`StopFailure.error` is one of eleven values:

    authentication_failed  oauth_org_not_allowed  account_on_hold  billing_error
    rate_limit             overloaded             invalid_request  model_not_found
    server_error           unknown                max_output_tokens

`SessionStart.source` is one of five: startup, resume, clear, compact, fork.
Subagents are **not** a session source; they get their own `SubagentStart` / `SubagentStop`.

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

    tool_calls:array(Lqb())}))),Nqb=Se(()=>YP().and(ye({hook_event_name:Ct("PermissionDenied"),tool_name:string,tool_input:json,tool_use_id:string,reason:string

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

    mcp_server_name:string,message:string,mode:enum["form","url"])?,url:string?,elicitation_id:string?,requested_schema:map(string,json)?}))),Qqb=Se(()=>YP().and(ye({hook_event_name:Ct("ElicitationResult"),mcp_server_name:string,elicitation_id:string?,mode:enum["form","url"])?,action:enum["accept","decline","cancel"]),content:map(string,json)?}))),e6b=["user_settings","project_settings","local_setting

### ElicitationResult

    mcp_server_name:string,elicitation_id:string?,mode:enum["form","url"])?,action:enum["accept","decline","cancel"]),content:map(string,json)?}))),e6b=["user_settings","project_settings","local_settings","policy_settings","skills"],t6b=Se(()=>YP().and(ye({hook_event_name:Ct("ConfigChange"),source:Dr(e6b),file_path:string?

### ConfigChange

    source:Dr(e6b),file_path:string?

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

    turn_id:string,message_id:string,index:number,final:bool,delta:string})).describe("Hook input for the MessageDisplay event. Fired with each batch of newly completed lines while an assistant message streams. Display-only: the stored message and what the model s

### SessionEnd

    reason:p6b()
