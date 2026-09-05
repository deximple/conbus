# Handoff — 2026-09-06

Written for whoever picks this up next. The design phase is finished, the recorder is built, and it is installed and
recording on this machine. Read this, then read the plan.

## Where things are

- Repo: `/Applications/conbus`, remote `deximple/conbus`, **private**. `main` is `4d1839f` (PR #11 merged).
  The installed binary and `sh.conbus.daemon` are build `4d1839f76`. Do not overwrite that inode in place:
  write beside, rename, then `launchctl kickstart -k gui/$UID/sh.conbus.daemon`. `conbus doctor` prints both
  stamps and says `DIFFERS` when they do not match. Next increment is `feat/mirror-permission-notification`.
- The live machine, same moment: 46,256 records in 13 segments (96 MB), 3,886 conversations, 0 unreadable;
  Claude Code hooks 11/11 registered, Codex hooks 6/6 registered and trusted; 17 twin sessions all bound to their
  live side, 2 of them split before the twin rule existed; 578 drops, most of them Cursor camelCase events arriving
  under Claude Code's name (`preToolUse` / `postToolUse` / `beforeSubmitPrompt`). Codex history is in the
  corpus: 277 threads, 2,783 turns, 5,759 records, applied 2026-09-03 08:02 KST. Last *hooked* Codex record is a
  `session-end` on 2026-09-04; Codex.app desktop has been running and burning the same account since.
- Codex quota, same moment: `plan plus · 5h 100% used · weekly 76% · reset credits 3 · EXHAUSTED (rate_limit_reached)`.
  That is the first live exhaustion. See item 7 under "Not done, and next".
- The plan is the single source of truth: [`.cursor/plans/shared_workspace_context_3f7139b8.plan.md`](.cursor/plans/shared_workspace_context_3f7139b8.plan.md). The filename is a generated ID and no longer matches the content; leave it alone so the Cursor plan UI keeps its association. A reader-facing copy under `docs/` gets generated when the repo goes public.
- `cargo 1.94.1` and `rustc 1.94.1` are installed on this machine, so edition 2024 is available.
- A cargo workspace exists: `crates/conbus` (the binary) and `crates/conbus-core` (the lib). Edition 2024.
  330 tests as of 2026-09-06 (137 + 2 + 5 + 14 + 28 + 144), `cargo clippy --all-targets -- -D warnings` clean. See "Next task" for what is built and what is not.

## What Conbus is, in one paragraph

One conversation keeps its identity while the coding harness underneath it changes. Claude Code hits its five-hour limit mid-task, the next turn runs in Codex, and nothing gets re-explained. The bus owns the conversation state, the ID, and the routing; the vendor CLI stays exactly as the user knows it. The mechanism is a hook sidecar that mirrors every vendor session into an append-only log, plus injection of a hydrate pack built in memory. Positioning is insurance, not savings: *this conversation survived N vendor limits, policies, and outages, with zero conversation loss.*

## Decisions that are closed — do not relitigate

All of these were argued through four review rounds. The reasoning is in the plan; the conclusions are here so you don't reopen them.

1. **The bus owns state and ID; vendor TUIs stay the window.** Stage 0 does not build a chat UI. Building one is what kills the product, because the vendor TUI is better than anything we'd ship in four weeks.
2. **Stage 0 is Claude Code ↔ Codex only.** Cursor is excluded, not because it lacks hooks but because it has no structured quota signal, so automatic routing can't be proven there.
3. **Stage 0 dual-warm is asymmetric: only the Codex leg stays warm.** Claude stays interactive. Claude's warm primitive (`shouldQuery: false`) runs through the Agent SDK, which is the exact path Anthropic's paused credit-separation policy targets. Symmetric dual-warm and the permission proxy are stage 1.
4. **Stage 0 keeps four weeks by cutting scope**, not by extending. Moved to stage 1: the control plane, the arena implementation, full multi-harness install, gap filling. Kept: recorder, injection hardening, probes, pack/log schema, asymmetric warm, router, rolling summary, lease, survival meter, `doctor`, fact schema as a document.
5. **The rolling summary cannot be cut**, and it must never run on the leg it is protecting. `claude -p` draws from the subscription limit, so summarizing with Claude burns the quota we're trying to preserve. Default backend is the opposite vendor's low-effort model.
6. **Retention (1.5) is pulled into stage 1**, which drags envelope encryption and the cross-platform KEK abstraction with it. Shipping default stays "conversation only" (tool bodies not written to disk).
7. **Name is Conbus** — conversation bus. Binary `conbus`, conversation ID stays `bus_id`. crates.io and npm names are free and deliberately unclaimed; publishing a placeholder would announce the name before the launch window. GitHub org `conbus` is taken by an unrelated 2015 org, hence `deximple/conbus`. Domain is deferred to just before MVP, `conbus.sh` leading.
8. **Implementation:** one binary with argv dispatch (`conbus daemon|hook|log|install|backfill|doctor|quota|hop|--version`), cargo workspace internally. JSONL segments are the source of truth, SQLite is a **disposable** derived index that can always be rebuilt from segments. Data lives in `~/.conbus/`, respecting `$XDG_STATE_HOME`.

## Hard constraints — violating these breaks the product legally or technically

- **Never read, store, or intermediate vendor auth tokens.** Login happens only through the unmodified vendor binary's own flow. This is not caution; it is the observed line. Every project that touched tokens got shut down, and every project that spawned unmodified binaries as children did not.
- **Never modify vendor binaries** or strip their auth.
- **Never pool credentials or quota across people.** Routing policy is configuration, not service resale.
- **Do not parse vendor session files on the live path.** Claude's docs say the transcript format is internal and changes between releases. Mirroring, gap filling, and routing use official channels only. Backfill is the single exception, and only with a fixture-protected parser that skips unknown schemas.
- **Never inject into a Codex thread the user opened.** Codex has no primitive to delete a preserved rollback source, and rolled-back prompts have been observed resurrecting after restart. Our warm thread must be one we opened via app-server.
- **Do not expose a secret-reading tool to the model.** Secrets are injected as child env at spawn.
- **Do not use vendor trademarks in product or feature names.**

## Known vendor quirks — these are already paid for, don't rediscover them

- Hook output strings cap at **10,000 characters**. Over that, the content is written to a file and the model gets a path plus a short preview. **The failure is silent** — it looks like it works if headers land in the preview. Assert inline delivery; treat `<persisted-output>` in a transcript attachment as a failure.
- `SessionStart` fires multiple times per project within hundreds of milliseconds and some of those are ghosts with no transcript (`anthropics/claude-code#78455`). **Mint `bus_id` on the first `UserPromptSubmit`, not on `SessionStart`.** SessionStart only registers a candidate.
- Transcripts are written asynchronously. On `Stop`, wait briefly (under 2s) for the last assistant message to appear before recording.
- `SessionStart` matchers are `startup`, `resume`, `clear`, `compact`, `fork`, and the hook re-runs on resume — so a fresh pack can be re-injected on every resume, and `compact` is the hook for re-injecting right after compaction. Mid-session events replay stale saved text instead of re-running, so stamp the pack with its creation time.
- `initialUserMessage` is headless-only in practice. The interactive injection channels are hook stdout / `additionalContext` and the system-prompt flags. Nothing else.
- Claude Code **Desktop**'s `/clear` omits `source` (`anthropics/claude-code#76704`). Document it, don't work around it.
- Codex hooks need **absolute** node paths because VS Code's app-server has a different PATH. The plan said `Stop` hooks need `async: true`; they do not — conbus registers no async hook, 0.147.0 dropped them, 0.148+ allows them again.
- Codex quota lives on `account/rateLimits` via app-server, and there is no `StopFailure` equivalent in Codex hooks. Rate limits are **account-scoped**, so a sidecar `codex app-server` polling `account/rateLimits/read` sees consumption regardless of which process burned it. **This is probe #1** — if it doesn't hold, stage 0 is one-directional failover only and the docs must say so.
- Global hooks will mirror our own background `-p` sessions. Mark them with an env variable at spawn and have the hook exit immediately when it sees the marker.

### The one that cost a session — found by installing it, not by testing it

**A hook that writes nothing to stdout coincided with the vendor session going down.** Registered for real,
`conbus hook` exited zero with zero bytes on stdout, and the client then failed with
`400 messages.983.content.1.text: cache_control cannot be set for empty text blocks`. Reverting the registration
fixed it; re-checking after the revert confirmed it. The recorder was breaking the conversation it exists to
preserve — the single rule this whole client is built around.

**The correlation is solid; the mechanism is not confirmed, and this file used to state it as though it were.** A
later pass through 2.1.241's own bundle could not find the path that turns a zero-byte hook stdout into an empty
text block: `parseHookOutput("")` yields a `hook_success` attachment with empty content, and three separate guards
drop it — the prompt path (`if(!content) break`), the model-side renderer (`if(e.content==="") return []`) and a
`"(no content)"` substitution. So the causal story is a hypothesis that fits the evidence, not a read fact. It
changes nothing about the fix — printing well-formed JSON is free, correct, and now carries the briefing as well —
but a confident wrong mechanism is exactly what this file exists to stop.

Two things are worth carrying forward more than the fix itself.

- **"The hook writes 0 bytes" and "the vendor receives nothing" are different claims, and only the first was
  measured.** Every path was checked for empty stdout and exit zero, and that was reported as proof the hook was
  harmless. It measured our side of the pipe. What the vendor does with a zero-byte stdout on a context-injecting
  event was never measured, and it is the half that mattered.
- **Nothing in the test suite could have caught it.** The tests drive the real binary against a real socket, and
  they all passed. The failure needed a real vendor session, which is exactly what AGENTS.md says verification here
  usually means, and it was skipped for the parts that looked obviously safe.

The fix: every path now prints `{"continue":true,"suppressOutput":true}`, the vendor's own documented JSON control
shape, which cannot be mistaken for an empty context block and injects nothing.
`the_hook_always_says_something_well_formed_on_stdout` pins it across all six paths, including the failing ones.

**This fix is not yet confirmed against a real session.** It is a hypothesis with a mechanism, in the same position
the wrong conclusion above was. It is confirmed when the side chat that failed opens cleanly with the hooks
reinstalled, and not before.

### Read off the installed CLI on 2026-08-30 (Claude Code 2.1.241)

The full extracted contract — 31 payload schemas — is in
[`crates/conbus/tests/fixtures/claude_code_2.1.241_contract.md`](crates/conbus/tests/fixtures/claude_code_2.1.241_contract.md),
next to the real traffic that pins it. Read that before touching the adapter. The headlines:

- **`StopFailure`'s reason field is `error`.** `failure_reason` also appears in the binary and is *not* the payload
  field. The first version of this adapter used it, produced `reason: "unspecified"` on a real failure, and looked
  like it worked. `strings` on a binary is not a contract; the embedded schema definitions are.
- **`error` has eleven values**, not four: `authentication_failed`, `oauth_org_not_allowed`, `account_on_hold`,
  `billing_error`, `rate_limit`, `overloaded`, `invalid_request`, `model_not_found`, `server_error`, `unknown`,
  `max_output_tokens`. There is no `credits_required` — that string exists in the binary but is not one of these.
  Only `rate_limit` and `overloaded` mean "this leg cannot serve the turn right now"; `invalid_request`,
  `model_not_found`, and `max_output_tokens` are faults in the request that routing does not fix. **The router will
  want a three-way split (capacity / account / request fault), not the two-way one the recorder ships with.** The
  recorder keeps the vendor's string verbatim so that is a re-read of the log, not a re-run of the traffic.
- **`Stop`, `StopFailure`, and `SubagentStop` carry `last_assistant_message`**, described in the schema as: *"Text
  content of the last assistant message before stopping. Avoids the need to read and parse the transcript file."*
  **The plan's sub-2s flush barrier on `Stop` is not needed and should be struck** — the vendor hands us the text.
  But do not overstate it, as an earlier version of this file did: it is the text blocks of the **last** assistant
  message only, it collapses to absent when that message is pure tool use, and intermediate assistant text inside a
  multi-step turn is not on `Stop` at all. Holding the whole conversation would mean mirroring `MessageDisplay`
  (`turn_id`, `message_id`, `index`, `final`, `delta`), which is per-streamed-line-batch and needs its own retention
  decision before it goes anywhere near the default "conversation only" level.
- **`prompt_id` is on the common base** — "UUID correlating a user prompt with all subsequent events until the next
  prompt", and the same value as the `prompt.id` OpenTelemetry attribute. Recorded as `vendor_turn_id`, and read only
  from `UserPromptSubmit`, `PostToolUse`, `Stop`, `StopFailure`. It is **one slot on a process-global request
  journal**, so it is stale on the session-lifecycle events: `SessionStart` on resume and fork fires before the reset
  and carries the previous conversation's value, compact never resets, and `SessionEnd` names the turn that just
  finished. It also does **not** de-interleave subagents — it is the same value for a subagent and its parent.
- **`agent_id` is on the common base too**, and it is the field that does what we wanted `prompt_id` to do. A
  subagent's `PostToolUse` arrives under the **parent's** `session_id`; the agent identity travels only here. Without
  it the log silently interleaves two agents under one conversation. Recorded as `vendor_agent_id`.
- **`UserPromptSubmit` has a `source`** in the schema — `user`, `sdk`, `system`, `loop_wakeup`, `schedule_wakeup`,
  `poll_event` — **and 2.1.241 never sends it.** Both emit sites have the spread compiled out, and a real `-p` run,
  which must be `sdk`, produced a `UserPromptSubmit` with no `source` key. The schema's own note reads "payloads may
  omit it while the field rolls out". It is parsed and recorded as `author` when present, and nothing branches on it.
  It is **not** a self-exclusion signal and could not replace `CONBUS_INTERNAL_SESSION` even if it were emitted:
  `sdk` also covers the user's own deliberate `claude -p` runs and `loop_wakeup`/`schedule_wakeup` are the user's own
  automation, so excluding on it would delete real conversations from an append-only log. It appears on one event, so
  it could not suppress that session's `SessionStart`, `PostToolUse`, or `Stop` either. The env marker stays the
  exclusion mechanism; `author` is what will let the survival meter separate human turns from machine ones.
- `SessionStart.source` is five values — `startup`, `resume`, `clear`, `compact`, `fork`. **Subagents are not a
  session source**; they get their own `SubagentStart` / `SubagentStop` with `agent_id` and `agent_type`.
- `PostToolUse` carries `tool_use_id` and `duration_ms`; `PostToolUseFailure` carries `error` and `is_interrupt`.
- `PostCompact` carries `compact_summary`, which is the obvious input to re-injection after compaction.
- **`mlockall` returns ENOSYS on macOS** (errno 78). The plan's "private memory, mlock" is not satisfiable
  process-wide on the primary dev platform. Per-region `mlock(2)` does work on Darwin, but using it means putting
  derived state in an explicit arena rather than on the ordinary heap. Until then `conbus doctor` reports
  `memory_locked=false` with the errno rather than claiming a guarantee we do not have.

### How to get this again

`claude -p` inside a sandboxed child failed OAuth refresh, and that failure was itself useful: it produced a real
`StopFailure`. The four captured payloads are the fixture. To capture more, register a hook that tees stdin to a file
and run one session; the schemas themselves come out of the binary with a search for `hook_event_name:Ct("`.

## Next task

`recorder-first`. One week, **recording only, no injection** — that was the plan; the briefing shipped on 2026-09-01 and is live on both harnesses as of 2026-09-03. Injection hardening is a separate item because the
quirks above take weeks, not days.

Steps 1–4 of the suggested order are built; step 5 is not.

**Done.**

1. Cargo workspace — `crates/conbus` (binary) and `crates/conbus-core` (lib), edition 2024, two crates and no more.
2. Event model and JSONL segment writer. Every record carries `v` (schema version) and a harness/version stamp.
   Records are one flat line each so a segment stays greppable. Timestamps are fixed-width UTC so sorting a segment
   as text gives append order. Retention is applied *inside* the writer, which is the only path to disk, so no caller
   can write a tool body the workspace said to withhold. Segments roll over at 8 MiB and a line torn by a crash is
   reported as a defect rather than swallowed — and, more to the point, the next append heals the missing newline
   instead of being glued onto the torn line and lost with it.
3. Unix socket daemon (`conbus daemon`), single-threaded, registry in ordinary process memory. Core dumps are
   disabled. Memory locking is attempted and **reported**, not assumed — see the ENOSYS note above. Binding refuses
   to steal a socket a live daemon still owns and reclaims one a dead daemon left behind.
4. `conbus hook` — a thin client that reads a payload on stdin and exits **zero no matter what**, because a non-zero
   exit from `UserPromptSubmit` blocks the user's prompt. A dropped record is written to
   `~/.conbus/drops.jsonl` and surfaced by `doctor` — stderr is a bad channel for it, because the vendor swallows it
   in normal use and when it does not it becomes a transcript attachment, so a standing fault like a stopped daemon
   writes one per hook per turn into the conversation it exists to protect. stderr speaks once per distinct reason
   per minute, keyed on when it last *spoke*, so a standing benign fault can never silence a new one.
   The `CONBUS_INTERNAL_SESSION`
   marker makes it exit before reading anything, so our own background sessions are not mirrored.
   Also `conbus log` and `conbus doctor`.

`bus_id` minting follows the rule: `SessionStart` only registers a candidate, the first `UserPromptSubmit` mints and
binds, and the remembered session start is then flushed into the log *behind* the prompt that proved the session real.
A prompt mints on its own if the session start never arrived, so Desktop's `/clear` does not cost a conversation.

**Installed, and queryable.**

`conbus install` registers the hook with both harnesses — `~/.claude/settings.json` for Claude Code and
`$CODEX_HOME/hooks.json` for Codex, which is **not** `settings.json`; that path exists in Codex only inside its
one-shot import of Claude sessions. It merges rather than overwrites, backs up first, is idempotent, and registers
by absolute path because a vendor's PATH is not ours. It reports by default and writes only with `--apply`.

Two vendor facts it encodes, both measured against the running binaries and both contradicting the plan:

- **A Codex hook marked `"async": true` is dropped**, with a warning, by 0.147.0 — `hooks/list` returns ten events
  instead of eleven and `stop` is simply absent. The plan says Codex `Stop` hooks need `async: true`; following it
  would have silently cost every Codex turn-end record. Nothing conbus writes is async.
- **Codex has a trust gate that fails silently.** Registered-but-untrusted hooks do not fire and nothing is printed;
  the state is only visible through `hooks/list`'s `trustStatus`. A human must approve in Codex itself.
  `conbus install` says so and deliberately does not write the trust hash — `hooks/list` hands you the hash it
  wants, so forging consent would be trivial, which is exactly why it must not be done.

`conbus log` takes `--bus-id`, `--since`, `--tail` and `--grep`. Before this it printed the entire store, which
stops being usable at the first backfill.

**It is installed, supervised, and recording. The paragraph that used to sit here was wrong.**

It said both settings files had been restored byte-identical and that no conbus entry remained in either. That was
true of `~/.claude/settings.json` and **false of `$CODEX_HOME/hooks.json`**, where six conbus entries were left
registered — `PreToolUse`, `PostToolUse`, `SessionStart`, `SessionEnd`, `UserPromptSubmit`, `Stop`. Both files
carry conbus again now, deliberately.

**The six Codex hooks were trusted on 2026-09-03 and have run.** A Codex hook does not run until a person approves
it inside Codex; it is not executed-and-ignored, the process is never spawned. Codex keeps the consent record as
`trusted_hash` under `[hooks.state."<key>"]` in `config.toml` — keyed by hooks file, snake-cased event,
matcher-group index and position within the group. Until 2026-09-03 there was no record for any conbus entry, and
`conbus doctor` said so, because the executability probe cheerfully reports "runs" about a command Codex will never
invoke, which is the same shape of lie as an empty drop log. conbus reads that record and must never write it:
`hooks/list` hands out the very value that would forge it. The gesture that grants it is the CLI's startup dialog
("Hooks need review — 19 hooks are new or changed — Trust all and continue"), which reviews every untrusted hook in
every file Codex loaded at once — so trusting conbus's six also trusted paseo's five and two bundled plugin hooks.
Within the hour the log held its first Codex conversations, and a Codex session in a checkout with history was
briefed and quoted the record back. See `tests/fixtures/codex_0.152.1_contract.md` for what the traffic taught.

Two upgrade traps follow from the same hash. It covers more than the command string, so any edit to a conbus entry
flips it from `trusted` to `modified` — and a modified hook silently stops running. And inserting a hook ahead of
an existing one shifts its group index, changes its key, and orphans its trust record outright.

**When it was installed, it was supervised and recording.**

The binary lives at `~/.local/bin/conbus` — outside the worktree, so tidying a branch does not break the hooks.
`conbus install --apply` registered ten events with Claude Code and six with Codex (eleven since `SubagentStop` was added; 11/11 and 6/6 on this machine as of 2026-09-03), merging into settings files
that already held fourteen and five of the user's own hook events respectively; every pre-existing key and hook came
back byte-for-byte and still first. A `launchd` agent (`sh.conbus.daemon`, `RunAtLoad` + `KeepAlive`) supervises
the daemon; killing it `-9` gets it restarted, which was checked rather than assumed.

**The corpus is 23,286 backfilled records over 3,425 conversations, spanning 2026-04 to 2026-08, plus live traffic
from the session that installed it.** Bootstrapping the agent requires stopping a manually started daemon first —
otherwise the agent's daemon hits the "already listening" refusal and `KeepAlive` turns that into a restart loop.

One consequence worth knowing, and it is the design working: backfill adopts a session file's own UUID as the
`bus_id`, so when the daemon restores bindings it recognises a *live* session whose history was backfilled and
continues the same conversation. A resumed session's past and present are one `bus_id`, not two.

**The same mechanism ran the other way, and it split two conversations.** Backfill keyed idempotency on what
backfill had written, not on what the recorder had, so every session the recorder had already mirrored live was
recovered a second time under the transcript's UUID — fourteen twins on this machine, thirty more one `--apply`
away. And because `--apply` runs with the daemon stopped and appends its copy *after* the live records, "later
record wins" bound each of those sessions to the copy when the daemon came back. Two sessions that were still open
kept going there — `1ce6bfb1…`, minted live twenty minutes before the third backfill, and `9f8b6bf2…`, resumed the
next day — so each is one conversation under two `bus_id`s, and nothing un-splits an append-only log. Fixed on
2026-09-02 at both ends: `Registry::restore` never lets a reconstructed record displace a live binding, and
backfill leaves alone any transcript whose session the recorder has seen (the dry run here now declines 44 and
recovers nothing). `doctor` prints the twins and which side each is bound to — 14 twins, 14 live, 2 split — by
running the same restore the daemon does, so the two cannot disagree.

**Codex is registered, trusted and recording** (Codex 0.152.1, since 2026-09-03). What its first traffic taught,
in order of how much it changed:

- **Codex fails a hook over fields it does not take.** `PreToolUse` rejects `continue`, `stopReason` and
  `suppressOutput`; `PostToolUse` rejects `suppressOutput`. The Claude-shaped silent reply produced "hook (failed)"
  twice per tool call in the TUI while every record landed. The hook now answers Codex with `{}` on every silent
  path and drops the two control fields from the briefing; Claude Code keeps its reply, because a zero-byte stdout
  once took a session down there.
- **The briefing reaches Codex's model.** `systemMessage` + `hookSpecificOutput.additionalContext` on
  `UserPromptSubmit` was shown in the TUI and quoted back by the model. Model-visible hook output is capped at
  ~2,500 tokens (`additionalContextLimit`), above the pack budget.
- **Codex spawns a memory-writing agent beside a session**: its own `session_id`, `transcript_path: null`, cwd
  `~/.codex/memories`, a 50 KB instruction prompt beginning `## Memory Writing Agent`, tool calls, `SessionEnd`, no
  `Stop`. The recorder mints it as a conversation in the memory workspace; nothing marks it machine-authored yet.
- **`SessionStart` fires at the first prompt, not when the TUI opens**; a session closed without a prompt fires
  `SessionEnd` alone, which the daemon can only orphan. `tool_use_id` is `exec-<uuid>`; `tool_response` is a string.
- Async hooks are allowed from 0.148 (0.147.0 dropped them). `codex exec` hung at MCP start-up here and is
  unverified as a hook source. Project-local `.codex/hooks.json` loads only after the directory itself is trusted.

**Codex history is recovered too, through the app-server.**

`conbus backfill` also asks a fresh `codex app-server` for `thread/list`, page by page, and `thread/turns/list` per
thread, and folds each thread into a `session_candidate` plus a started/ended pair per prompt, stamped
`backfilled_from: codex_thread` with the thread's `cwd`, `source` (`cli`, `vscode`) and `cliVersion`. The thread id
equals `sessionId` equals the hook's `session_id`, so it is the `bus_id`, and the two rules the transcripts follow
hold here: a thread the recorder saw live is left alone (4 of 281 on this machine), a thread recovered once is not
recovered twice. One shape needed the transcript parser's rule: **the VS Code extension's legacy history splits one
exchange into a prompt turn and reply-only turns** — 590 of 1,070 turns in the 60 most recent threads carried
nothing but `agentMessage` items — so a reply belongs to the most recent prompt and the last reply ends it. A reply
before any prompt (a forked or resumed thread whose prompt lives in its parent) is counted, 121 of them, not
attached. On this machine: 277 threads from 2026-03-09, 2,783 turns, 5,759 records, in about two minutes, from
0.107.0 threads onward. The JSON-RPC client (`appserver.rs`) is shared with `conbus quota`; tests never start the
real server — a stand-in script speaks the captured shapes — because the first suite run that did recovered the
author's real Codex history into two temp logs.

**History is recovered.**

`conbus backfill` reads Claude Code's own transcripts — the one place the plan permits a vendor session
file to be parsed, because a backfill failure thins the corpus while a live-path failure loses the
conversation happening now. On this machine it recovered **3,425 conversations, 12,025 turns and 23,280
records from 5.24 GB in four seconds**, into a 78 MB log. It is idempotent two ways: the session file's own
name is a UUID, so it *becomes* the `bus_id`, and a second run recovers nothing rather than a second copy; and
a transcript whose session the recorder mirrored live is left alone, because a copy of it would be a second
conversation for the same session (the twins above). A dry run reads beside the running daemon; only
`--apply` needs it stopped. Note that 641 of the transcripts backfilled on 2026-08-31 were gone from disk two
days later — the vendor prunes them — so the corpus already holds conversations nothing else does.

It reads three line shapes and counts everything else by reason — 515k attachments, 88k tool-only
assistant replies, 54k tool-result user lines. Tool traffic is deliberately not recovered: it is 90-97% of
the bytes and the default retention level would elide it anyway.

Three things the transcript has that the live hook path does not:

- **The vendor version**, on every message line. 39 distinct builds across this corpus. The hook payload
  carries none and no environment variable exposes one, so backfilled records are the only ones with a
  version stamp.
- **`promptSource`**, the author field the hook has compiled out. And its vocabulary is *larger* than the
  hook schema's six values — the corpus contains `typed`, `queued` and `suggestion_accepted`. Whatever
  the two fields' relationship is, it is not identity. This is why `Author` is an open string.
- **When it happened.** `Entry` now carries `observed_at`, and a record that knows its own time keeps it
  rather than being stamped with the moment it was read.

Everything recovered is stamped `backfilled_from`, because a reconstructed record is a weaker fact than a
mirrored one and a reader that cannot tell them apart will over-trust the log.

**Two gaps in the design, not in the code.**

These came out of a question the design could not answer: *the same workspace is open in Claude
Code, Codex and Cursor at once, each with its own vendor session id and genuinely different
content — how does a common conversation get made, and how does a session join it?*

**1. There is no way for a session the user opened to join a conversation.** `bus_id` is minted per
`(harness, vendor_session_id)`, so three concurrent sessions are three conversations. That part is
correct and should stay: their content differs, and merging three trains of thought into one
append-only log cannot be undone. But the design's answer to "how does the next harness continue
the conversation" is *the bus opens that session itself* — `--session-id <uuid>` on Claude Code, an
app-server thread the bus owns on Codex — and that only covers legs the bus spawns. A session the
user opened has no route in. The plan never says so.

**2. The bus has no authority over a vendor's desktop app.** It can spawn CLI and headless children
and nothing else. There is no API by which it opens a workspace or starts a session in Claude
Desktop or the Codex desktop app, and driving the GUI is not something this project will do. So
"the bus owns the leg" holds for the automatic router path with a headless leg — which is exactly
why the plan's stage-0 asymmetric warm works — and does not hold anywhere a person is actually
sitting. For those surfaces the bus is a passenger.

Three vendor facts that constrain any fix, all read off the installed binaries:

- **The two vendors cannot share a session id.** Claude Code takes `--session-id <uuid>`; Codex's
  `ThreadStartParams` has no `threadId` at all — Codex assigns them. So identity is carried by the
  registry's alias map, which is what the code already does, and never by making the ids equal.
- **A command run inside a Codex session cannot learn its own thread id.** Claude Code sets
  `CLAUDE_CODE_SESSION_ID` in a child's environment; Codex sets no equivalent. Anything
  user-invoked on the Codex side has to identify its session some other way — process ancestry is
  the only exact one; cwd plus an open turn is ambiguous in precisely the concurrent case above.
- **A pack delivered as ordinary tool output needs no vendor injection primitive.** If the agent
  runs a command and the pack arrives on stdout, it is a tool result like any other: no
  `inject_items`, no rollback-source hazard, no 10,000-character `additionalContext` cap. That is
  worth knowing even though it must not become the primary path.

**And a correction to what the answer should be.** The first design drawn from this put a manual
`conbus resume <bus_id>` at the centre, because it is the cheapest thing that works everywhere. That
is backwards. A design where a person types a command to continue their own conversation has no
wedge — the vendor's own `/import` already does a worse version of that, by hand, and nobody uses
it. **Manual has to be the last resort, and the automatic path has to be invisible enough that
nobody thinks about it.** What that automation actually is, is being designed now; the entry below
is the shape it has to fit.

**How both gaps close: lineage, not adoption; and a gate that refuses by default.**

The question "which conversation does this session join" has no safe answer, and asking it was the
mistake. `bus_id` answers *whose turns are these*; it should keep answering only that. What a
continuation needs is a second, weaker relation — *what came before this* — and the two were
conflated because only the first existed.

- **Merging is dead.** Adopting an existing `bus_id` for a session the user opened cannot be
  undone: identity *is* the write, so two trains of thought interleave under one id forever. The
  concurrent case makes a wrong merge routine rather than rare — same checkout, all live, genuinely
  different content. And it erases the project's own kill criterion: if sessions in a workspace
  share an id there are no hops to count, so "routing hops per week" reads zero while the metric
  still looks healthy.
- **An edge is recorded, so it can be wrong.** A `continues` record under the *successor's* id
  moves nothing and can be contradicted by a later line. Its evidence field is the plan's hop
  taxonomy: `displaced` for a failover hop, `handed_off` for a routing hop, counted separately as
  the plan asks.
- **Two modes, sharing a pack builder and nothing else.** *Directed* is a leg the bus spawned —
  identity is known because we started the process for that reason, so there is no gate and no
  guessing. *Ambient* is a session the user opened — identity is unknown, the gate runs, and the
  default answer is no. The invisible case never guesses; the guessing case is tuned so its visible
  failure is "it did not help", never "it lied".

*(Superseded on 2026-08-31 by the briefing decision in `continuation.rs` — it narrates the checkout instead of choosing a conversation, and its seven silence reasons replace the rules below; "What a person sees" further down is current. Kept for the reasoning.)*

**The gate is a refusal engine with one positive signal.** It runs on the session's *first*
`UserPromptSubmit`, never on `SessionStart` — that event has no prompt to judge and fires several
times per project, some of them ghosts, so paying a pack there is paying on ghosts. It requires all
of: a single candidate in this exact workspace, a different harness, live-authored (an edge is a
live-path fact, not a reconstruction), at least two turns, and a workspace that has gone quiet.
Ties are never broken — two candidates means inject nothing and record `ambiguous`. The one strong
positive signal on the whole surface is a predecessor that ended `interrupted_quota` or
`interrupted_policy`: it *announced* it could not continue. Everything else is a short recency
window. Refusals are recorded too, by name, because without them "it never fires" and "the gate is
too tight" are the same observation.

**The measurements that shape it, from the real log** (24,060 records, 3,434 conversations):

- **92.6% of conversations are one turn** — 3,179 of 3,434. The 255 multi-turn ones hold 8,862 of
  12,041 turns. Requiring two turns removes nine tenths of the candidates before any other test.
- **Only 2 conversations carry a workspace.** The live path now stamps every record, but the 3,425
  backfilled ones predate that and backfill emitted no session record at all (fixed 2026-09-01: it emits one, with workspace and entrypoint). Until then the
  gate has no corpus to be tested against, which is the point of making it a pure function: it can
  be replayed over the whole log offline, and "over 3,434 real conversations it fires N times and
  picks wrong M times" is a claim available before a single hydration ships.

**Delivery, with the vendor's real numbers.** For a session the bus did not spawn, hook
`additionalContext` is the only automatic channel that exists. Claude Code 2.1.241 caps it at
10,000 **UTF-16 code units** (`if (e.length <= 1e4) return e;`), inclusive, and past that persists
the text and hands the model a `<persisted-output>` block with a 2,000-byte preview. That is a
`String.length` test, so the local check is `encode_utf16().count()` — not `len()` (UTF-8 bytes),
not `chars().count()`. `INLINE_CAP_BYTES` in `retention.rs` is a byte cap and reusing its idiom here
would be wrong in both directions. The budget is set far below the cap so degradation is
unreachable by construction rather than merely tested for.

Claude Code awaits the `SessionStart` hooks while building the first message list, so a pack lands
before the first prompt is answered. Twelve of its twenty hook outputs accept `additionalContext`;
plain stdout is shown to the model on only three events and discarded everywhere else. Codex is
much narrower — five of eleven events accept it, there is no `fork` source, and it has neither
`Stop` nor `PostCompact` injection, so `UserPromptSubmit` is its only per-turn channel. Codex's
`additionalContextLimit` is a real per-hook key but its default is **unverified**; the plan's
"~2,500 tokens" stands unconfirmed, so the pack builder takes a budget argument (one constant, `PACK_BUDGET_UNITS = 2_600` UTF-16 units, for both harnesses as of 2026-09-03; Codex's ~2,500-token cap sits above it) rather than
assuming the two vendors are symmetric.

One correction to the note above: `<persisted-output>` is the right oracle but the wrong detector.
A hook cannot see the model's context, finding the marker means reading a transcript — banned on the
live path — and ordinary large tool results produce the same marker, so it is not specific. Assert
the budget locally on every emit; use the marker only in a probe against a session we own.

**What a person sees, and how they stop it** (as built, 2026-09-03). One `systemMessage` line on the turn
that was actually briefed, naming the conversation recounted — its first 13 characters, because 8 collide
within a 65-second window and three scheduled tasks minted in one second — and the command to inspect it;
a field both vendors have, separate from `additionalContext`, that costs no model tokens. The first live
briefing named the reader's own brand-new session instead, which is how this sentence got checked. No
injection, no line, ever; silence is the default. `/clear` already means "forget this", so a session whose
flushed start says `source: clear` is not briefed — a gesture the user already makes rather than one we
invented, with the Desktop caveat above. The explicit off is `CONBUS_BRIEFING=off` in the environment the
vendor runs hooks in (not a command: a command would need a second process to consult, and the hook already
reads its environment), carried on the wire as `brief: false` so the daemon neither injects nor records a
briefing nobody received — off means no briefing, not no recording. A boring lever for the negative case;
the positive case must never need one: a missed briefing costs one sentence of re-explanation, a wrong one
costs credibility in every session opened afterwards. Two more things the briefing now says honestly: a
prompt that opens with a vendor's own tell that a program wrote it (`<scheduled-task`, `## Memory Writing
Agent`) is not briefed even when the entrypoint calls the session attended, and a conversation with both
recovered and live records is "partly reconstructed", not "not observed live".

**What the gate did on the real log** *(measured 2026-08-31, before the gate was replaced in `897164c`; the numbers below are that measurement. `conbus doctor` now replays the briefing decision instead — as of 2026-09-06: 3,886 conversations judged, 115 brief, 100 silent/machine_authored; the replay histogram has the rest.)*

`conbus doctor` replays the gate over every conversation, judging each one against only what had
already happened when its first prompt arrived. On this machine's 3,443 conversations it fires
**zero** times and refuses 3,413 for `no_candidate`, 28 for `no_workspace`, 2 for
`workspace_too_broad`. Zero is the correct answer and also an uninformative one: every conversation
recovered from a Claude Code transcript is Claude Code and is a reconstruction, so the
different-harness and live-authored rules refuse the whole corpus before any other rule is
consulted. It proves there are no false positives and says nothing about discrimination.

So `doctor` prints a second, clearly labelled counterfactual with those two rules lifted — the
question this corpus *can* answer. There it fires **12 times out of 3,443**, refuses 3 for
`workspace_busy`, and — the result worth keeping — is **never ambiguous**, in a corpus whose largest
workspace holds 3,031 conversations. The recency windows, not the tie-breaker, are doing the
discrimination; the ambiguity refusal is a backstop that real data rarely reaches. No
`continue/displaced` appears at all, because a transcript carries no `StopFailure` and backfill can
only ever recover a turn that completed.

A trap worth recording, because it nearly shipped: the first counterfactual lifted only the
reconstruction rule, which on a single-vendor corpus changes nothing. It printed numbers identical
to the real gate and looked like a measurement. A counterfactual that silently equals the thing it
is contrasted with is worse than having none, and there is now a test that fails if it does.

**The briefing is built, and it reaches the model.**

The whole path runs end to end through the real binary and a real socket: on the first `UserPromptSubmit` of a
session the daemon decides, builds the pack, records that it did, and returns it; the hook frames it as the
vendor's context channel and prints it. The person gets one `systemMessage` line, which costs no model tokens.

**Six things about Claude Code 2.1.241's hook reply, read out of its bundle rather than assumed.** Each was
extracted from emit and consume sites at known byte offsets; `strings` found none of them.

- The shape is `{"hookSpecificOutput":{"hookEventName":"UserPromptSubmit","additionalContext":"…"}}`.
  `hookEventName` is **required and must equal the firing event**, or the entire hook result is thrown away —
  non-blocking, so the turn is fine and the injection simply never happens. Worse, the same failure is reported
  **once per session and then suppressed**, so a hardcoded name that drifted would show one error and go quiet
  forever. The firing event is therefore carried from the payload through to the reply.
- **`suppressOutput` does not suppress `additionalContext`.** In this build it suppresses nothing at all: its one
  runtime read is unreachable for any hook that emits JSON. What actually keeps plain stdout out of the transcript
  is emitting JSON in the first place.
- Exactly three things block the user's prompt: `"continue": false`, `"decision": "block"`, and **exit code 2** —
  the last regardless of what was printed. Every other non-zero exit, invalid JSON, a wrong event name, a timeout
  and a spawn failure are non-blocking. And a blocked turn drops the pack, since the block returns before the
  context is pushed.
- The cap is `String.length <= 10_000`, **inclusive, in UTF-16 code units**, applied **per field** — so the
  receipt does not spend the pack's budget. Past it the text is persisted and the model gets a `<persisted-output>`
  block with a preview.
- **The pack lands *after* the user's prompt, not before it.** This file used to say "before the first prompt is
  answered", which is true of the request and false of the position: the vendor appends hook context to the
  message list after the prompt message. Recency therefore favours our text over the user's own words, so
  precedence has to be *stated* in the pack rather than inferred from where it sits.
- `UserPromptSubmit`'s hook timeout is **30 seconds**, not the 10-minute global default, and a timed-out hook's
  partial stdout is parsed by nothing.

**And the marker is the right oracle but the wrong detector.** A hook cannot see the model's context, finding
`<persisted-output>` means reading a transcript — banned on the live path — and ordinary large tool results
produce the same marker. Assert the budget locally on every emit; use the marker only in a probe against a session
we own.

**Codex is narrower and its unit is different.** Five of its eleven events accept `additionalContext`
(`SessionStart`, `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, `SubagentStart`); there is no `fork` source and
no `Stop` or `PostCompact` injection at all, so `UserPromptSubmit` is its only per-turn channel. Its
`additionalContextLimit` defaults to **2,500 tokens**, and a token there is `ceil(utf8_bytes/4)` — so the real
inline budget is **10,000 UTF-8 *bytes***, the same number as Claude Code's in a different unit. A pack budgeted in
UTF-16 units is not automatically inside it. Also worth having: `turn/start` takes `additionalContext` as a map of
source id to `{kind, value}` with `kind` in `["untrusted", "application"]` — a native data-boundary marker, and
neither `inject_items` nor `developerInstructions`, so it carries no rollback-source hazard. That is the right
channel for a leg the bus spawns.

**Codex gives `SessionEnd` one second, and it is Codex's default rather than something conbus asked for** —
`hooks/list` reports `timeoutSec: 1` for that event where every other conbus entry reports 600, and the entry
itself carries only `command` and `type`. So the tightest deadline on the whole surface is a second, on session
teardown, and the hook client now bounds its own call to the daemon at 500ms. It had no timeout at all: a daemon
that is up but wedged would block the read forever, and the vendor's timeout would become the user's wait — 30
seconds on Claude Code's `UserPromptSubmit`, with the prompt stalled the whole time. The daemon's work per request
is a map lookup and an append, so anything near that bound is a fault, and losing one record is cheaper than
making somebody wait for it.

**Approval is per file, not per hook.** Codex reviews everything new or changed at startup, so approving conbus's
six also approves whatever else sits untrusted alongside them — five other hooks on the machine this was written
on. `doctor` says so, because being asked to consent without being told the scope is not consent.

**The daemon has a bound of its own now, because the hook's did not cover the daemon.** The client's 500ms
deadline bounds how long a *hook* waits; it says nothing about how long the *daemon* waits. The daemon serves one
connection to its end before accepting the next, and an accepted connection had no timeout at all — so anything
that connected and said nothing held every hook behind it in the backlog: until the vendor's own timeout before the
client deadline shipped, and after it, until each hook gave up at 500ms and lost its record instead. A read-only
audit on 2026-09-02 found it by reading, and the e2e that pins it
(`a_silent_connection_does_not_hold_the_hooks_behind_it`) failed first with exactly that shape — the hook behind a
silent connection exited zero after 506ms with `record dropped`. Accepted connections now carry a 200ms read and
write timeout, asserted at compile time to be shorter than the client's deadline, because the daemon has to give up
on silence before the hook gives up on the daemon or the bound protects nobody. A hook has its whole request ready
before it connects, so silence is never a hook that is thinking. A client that was answered and lingers is the
ordinary end of a connection; one that never spoke is reported on the daemon's stderr.

And the thing installing it proved, in the same hour: **Darwin refuses to set socket options on a peer that has
already hung up**, with `EINVAL`, and `doctor`'s liveness probe is exactly such a peer — it connects and leaves. The
first build of the bound reported every probe as `connection failed: Invalid argument (os error 22)`, one line in
`daemon.log` per `conbus doctor`, and the e2e suite could not have shown it because the test daemon's stderr goes
to `/dev/null`. A peer that left before it was served is nobody to serve; that case now ends the connection quietly,
and a unit test on a socketpair pins it.

One reading settled: the AGENTS.md rule against injecting into a Codex thread the user opened targets
`thread/inject_items` on the app-server. Returning `additionalContext` from a hook does not violate it — the plan
lists Codex `SessionStart additionalContext` as a sanctioned channel in the same breath. The rule's literal words
are broader than its subject; that is a wording gap to close in the text, not a reason to stop.

**How the design got from 3,445 conversations to 40 briefings, one measurement at a time.**

| after | brief | why the previous number was wrong |
|---|---|---|
| choosing a predecessor | 12 | an identity claim can lie, so precision had to be absolute |
| briefing a checkout instead | 3,259 | nothing is claimed, so nothing had to be refused — and it became chrome |
| only where somebody is reading | 168 | 3,096 of 3,445 conversations here are explicitly a program |
| only where they did not bring their own brief | **40** | the median opening prompt among the rest was 2,273 units |

Forty of the 349 attended sessions — about one in nine — naming 1.6 conversations each. `conbus doctor` prints the
whole distribution, replayed over the log, so the next person can move a constant on evidence rather than taste.

**Three things the pack refuses to do, each for a reason that cost something to learn.**

- **The fence is not forgeable.** The most dangerous field in a pack is not somebody's prompt, it is another
  session's *model output* — that model can be steered by anything it read, and its closing message lands here, at
  `isMeta`, positionally last. A static fence could be closed by quoted text, which would then continue in
  conbus's own voice. The fence carries a nonce taken from the id of the session being told, minted after every
  word the pack quotes was written, and quoted text is scrubbed of it and of any closing tag regardless.
- **It never truncates the assembled string.** The thing at the end is the closing fence, so cutting the string is
  how a quoting problem becomes an injection problem. Over budget it drops whole chapters, then refuses.
- **It carries the tail, not the head.** On the conversations this really fires for, the gap between the opening
  ask and the last activity was 9.5 days, 1.1 days and 10.4 days, while the window that makes them eligible is
  minutes. The head is a different piece of work wearing the same id.

**A vocabulary that was invented, and a gate built on it.** `Author` listed six values; four of them — `user`,
`loop_wakeup`, `schedule_wakeup`, `poll_event` — appear nowhere in 5,501 transcripts, and `is_human` was written
against the first, so it returned false for every turn ever recorded. What the vendor writes, over 61,899
main-thread prompts: `sdk` 9,184, `system` 139, `typed` 23, `queued` 1, `suggestion_accepted` 1, and 52,551 with
no `promptSource` at all. Then the correction to the correction: `sdk` is stamped on **hand-typed** prompts here —
one still carries the stray leading keystroke of somebody typing — so reading it as "not a person" would have
silenced exactly the sessions worth speaking to. Attendance is decided by `CLAUDE_CODE_ENTRYPOINT` alone, which
the hook reads from its own environment and backfill recovers from the transcript, and an unknown counts as a
person.

**The fourth harness identified itself.** The camelCase events arriving under Claude Code's name are **Cursor**:
the drop diagnostic now records a payload's field names, and one arrived carrying `cursor_version`,
`conversation_id`, `generation_id`, `workspace_roots` and `user_email`. Names only, never values — a payload's keys
are its schema, its values are somebody's conversation.

**Not done, and next.**

5. ~~**Codex backfill.**~~ **Done 2026-09-03**, through the official surface — see "Codex history is recovered too"
   below for the mechanism and the numbers. What the earlier version of this item measured on 0.147.0 (a
   `-32603 unknown variant` reading a newer thread) did not reproduce on 0.152.1, whose app-server read every thread
   back to 0.107.0. OpenCode `export` is untouched. **Running a backfill on the live machine** is a three-step
   dance with two traps: `launchctl bootout gui/$UID/sh.conbus.daemon`, then *wait until the socket stops answering*
   (the first `--apply` was refused with "a daemon is listening" because bootout had not finished), then
   `conbus backfill --apply`, then `launchctl bootstrap gui/$UID ~/Library/LaunchAgents/sh.conbus.daemon.plist` —
   which failed once with `Bootstrap failed: 5: Input/output error` and succeeded on retry. Every hook in that window
   drops as "no daemon" (2.5 minutes, three drops, on 2026-09-03). A dry run needs none of this: it reads beside
   the daemon.
6. ~~**The unmirrored events**~~ **`PermissionRequest` and `Notification` mirrored 2026-09-06.**
   The ask is `permission_asked`, never a denial. `Notification` keeps `notification_type`
   verbatim as `kind`; the quota-lifted spelling is not pinned until a capture lands.
   `MessageDisplay` stays unmirrored (retention decision first). Needs `conbus install --apply`
   on the live machine (13 Claude events, 7 Codex) and an atomic binary deploy.
7. ~~**A real quota exhaustion.**~~ **Arrived 2026-09-06 00:13 KST**, on the Codex
   account, not on a hooked turn. Probe #1 held in the direction that matters for
   the router: `conbus quota` printed
   `EXHAUSTED (rate_limit_reached)` without opening a thread. The redacted reply
   is `crates/conbus/tests/fixtures/codex_0.152.1_quota_exhausted.json`. Three
   things it taught, in order:
   - **The live `rateLimitReachedType` is `rate_limit_reached`.** A unit test had
     guessed `"primary"` from the window name. Wrong vocabulary; the parser is an
     open string so it still printed, but the fixture is what the router should
     match on.
   - **It is not a log event.** Codex hooks have no `StopFailure`; every hooked
     `Stop` is recorded `complete`. The whole corpus still has zero
     `interrupted_quota` records. A `continue/displaced` verdict still cannot be
     derived from the log — the counterfactual's "twelve handed_off, no
     displaced" stands.
   - **Desktop spent the quota the hooks never saw.** Last hooked Codex record:
     `session-end` on 2026-09-04. Codex.app (`152.0.7977.64`) had been up for
     hours. Same account, no hook traffic. The bus is a passenger there, which
     the design already said; this is the first measurement. Claude `StopFailure`
     is still the fixture that has to wait for a Claude limit.
8. **Delivery.** Shipped 2026-09-01 — the pack builder — pure, budgeted in UTF-16 code
   units, well under the 10,000 cap — then the `Briefed` event (the plan called it `Hydrated`) so an injection is a record rather than a belief,
   then the hook client learning a second reply beside `SILENT_OK`. Note the hazard in the client's own reply
   matching: it treats `Recorded | Deferred` as success and everything else as a defect, so a new response variant
   with no arm becomes a per-turn stderr diagnostic.
10. **Stage 0 close, 2026-09-06.** Router, extractive rolling summary, survival
   meter, hop-time Codex warm, and the fact-schema document. `conbus hop`
   decides from Codex `reached` / used% and from Claude `StopFailure`; the
   daemon writes `routed` on an `interrupted_quota` turn and `continued` when
   a briefing leads with a displaced conversation. `--apply` records the hop
   through the daemon (never a direct segment write) and, when the next leg is
   Codex, opens a bus-owned app-server thread and injects the extractive
   digest. A later `--apply` on a `warm:<id>` lease syncs that thread; any
   other holder still refuses. The hook path does not start an app-server —
   that bound is ~15s and would stall a prompt. Per-turn `inject_items` is
   therefore hop-time, not every Claude turn. Summaries stay extractive every
   8 turns; hop-path model calls stay 0. Fact contract: `docs/fact-schema.md`.
   **Not yet, and not required to close the wedge:** model-backed summary
   refresh, Claude quota probe, per-turn warm sync, a hop that opens a vendor
   window. The kill criteria start counting the day a `routed`/`continued`
   line lands on the live machine.
9. **The unknown fourth harness is Cursor, and it is the drop majority.** Identified
   2026-09-03 from field names (`cursor_version`, `conversation_id`, …). As of
   2026-09-06 about 385 of 578 drops are its camelCase events arriving under
   Claude Code's name — `preToolUse` / `postToolUse` / `beforeSubmitPrompt` /
   `stop`. Names only; values are somebody's conversation. Stage 0 still does
   not register a Cursor adapter. Do not guess a mapping from Claude's schema.

**Two live-machine failures worth not repeating.**

The recorder went silent for two hours and every check said it was healthy. The binary had been
overwritten in place while the launchd daemon held that inode mapped; macOS taints the vnode when a
signed binary changes under a mapping, so the kernel killed every exec of that path before `main`
ran — exit 137, no output, for `--help` as much as for `hook`. Claude Code kept firing hooks and
each one died instantly. **Install atomically**: write beside the target and rename, so a running
process keeps the old inode. And the drop log's emptiness proved nothing, because the drop log is
written by the hook: a hook that cannot start writes no drops. `doctor` now runs the registered
command, through a shell, marked internal, and insists on parseable JSON — and prints the most
recent record by time, which is the line the outage was visible in the whole time.

**The log's own repair split two conversations, and it was found by counting, not by testing.** Every
`backfill --apply` runs with the daemon stopped, and every session that is open at that moment comes back
bound to backfill's copy of itself, because the copy is the newest record for that session and restore took
the newest. The test suite was green throughout: `Registry::restore` had one test, with one record. What found
it was a survey of the real log keyed by `(harness, vendor_session_id)` — fourteen sessions held twice, two of
them with live records under both ids. Two rules follow. **A reconstruction never outranks what the recorder
saw**, in the registry and in backfill alike. And **run the survey, not the suite, when the question is about
the corpus**: `doctor` now carries that survey as its `twins` line, so the next person does not need the
script.

**`doctor` grew eyes for the three things the audits found by hand.** `build` on its first line and `daemon`
right after the socket: the binary names its commit (`conbus --version`, set by `build.rs`, `+dirty` when the
tree was not clean) and the daemon writes `daemon.build` beside its socket at start, so a binary renamed under a
running daemon reads `DIFFERS … restart the daemon` instead of nothing. `events` per harness says which of the
expected events the settings file does not register — the ten-of-eleven drift, in one line. And an orphan drop
now carries `detail` — the event name and the first eight characters of the session id, identifiers the log
already holds in full — so the drop log can tell a ghost session's `SessionEnd` from a lost tool call; the stderr
throttle still keys on the reason alone, so a restart does not put one line per orphan into the transcript.

**Where the milestone stands against its own success criteria.** "Every turn from both harnesses lands in the log":
the hooked surfaces, yes. Claude Code is confirmed on live traffic, including the `PostToolUse` this file spent
three revisions calling the most valuable missing capture. Codex hooks are trusted and have run — first live
traffic 2026-09-03, last hooked record a `session-end` on 2026-09-04. What still does not land is Codex.app
desktop, which burns the same account and never hits our hooks (see item 7). "The log survives a daemon restart":
yes, and now proven at 96 MB rather than argued. "Installed history is queryable": yes — 3,886 conversations,
with `--bus-id`, `--since`, `--tail` and `--grep`.

**Every tool request now has an answer in the log, and batches are marked.**

`PostToolBatch` fires once after every call in a batch resolves, "before the next model request". Its payload
repeats each call's name, input and response — all already on disk from the per-call events — so mirroring it
wholesale would put every body there a third time. What the log does *not* otherwise hold is which calls were issued
together and where the model's next request begins, so `tool_batch_ended` records **only the ids**, joinable to the
records that carry the substance. A batch line is about 200 bytes regardless of how much the tools moved.

The tool surface is mirrored end to end: `PreToolUse` → `tool_requested`, and then `PostToolUse` → `tool_use`
(succeeded), `PostToolUseFailure` → `tool_use` (failed), or `PermissionDenied` → `tool_denied`.

**But do not read that as "every request has an answer", which an earlier version of this file did.**
`PermissionDenied` has exactly one call site in the bundle and it is guarded by
`decisionReason.type === "classifier" && decisionReason.classifier === "auto-mode"` — so it fires for auto-mode
classifier refusals and **not** when a person simply answers no at the prompt. An ordinary user denial leaves a
`tool_requested` with no resolution. `PermissionRequest` is now `permission_asked` — the pre-decision
event, and the only place an ordinary refusal is visible. An unresolved request after that line
means: denied by a person, still in flight when the session ended, or missed by the daemon. The log
can tell the first of those from the other two only by adjacency, because the ask has no `tool_use_id`.

`tool_denied` is its own event rather than a third `ToolOutcome`, and the reasoning is the mirror image of why the
two post events *do* share one: `tool_use` means the tool **ran**. A call that never executed sitting in that set
would skew every aggregation over executions — durations, failure rates — unless each reader remembered to filter it
out. It also shares almost nothing with a result: no duration, no output, no error from the tool.

**The retention split is now explicit across the whole tool surface.** Tool bodies — a request's input, a result's
output, a failure's error text — are withheld at the default level, because that is where the sensitive material and
the volume live. Reasons the *harness* generates are kept: a denial's reason, a turn's interruption reason, a
session's end reason. Eliding a denial's reason would delete the only account of why a request never resolved, which
is the entire point of recording it.

**A tool call is now two records, and that is the point.**

`PreToolUse` is mirrored as its own event, `tool_requested`. It is *not* a third `ToolOutcome`: a request and its
result are two moments in time, and the log is append-only, so a later record cannot amend an earlier one. (The two
post events are different — they are exclusive descriptions of the *same* moment, which is why those share one
event with an outcome.)

What it buys, and what it costs:

- **`tool_use_id` is finally a join key.** Before this, a call produced exactly one record and there was nothing to
  join it to; the claim that the id "orders parallel calls" was unsupported and this file said so. Now issue order
  (`tool_requested`) and completion order (`tool_use`) are both on the log and differ visibly under parallelism.
- **A request that never resolves is visible**, which is a new kind of fact the log could not previously hold.
- **The input is recorded twice**, and deliberately: a `PreToolUse` hook is allowed to rewrite a tool's input, so
  "what was asked for" and "what ran" are separate facts that can legitimately differ. At the default retention
  level both are elided to a byte count, so the cost is small; at `Full` it is a real duplication for write-heavy
  sessions.
- **The log roughly doubles for tool-heavy turns.** That is the trade, taken deliberately.

`conbus log` prints the id as `#toolu_…` on both, because two parallel `Read`s render identically without it and
the join would be invisible in the view people actually read.

**A failed tool call is a tool call.**

The vendor sends `PostToolUse` **or** `PostToolUseFailure` for one call, never both, so a failure is not a second
kind of event — it is the same event with a different ending. `Event::ToolUse` therefore carries a `ToolOutcome`
(`succeeded` with an `output`, or `failed` with an `error` and an `interrupted` flag) rather than there being a
separate `tool_failed` variant. The reason is the shape of this project's bugs: a separate variant lets a consumer
handle only the successful half, and forgetting is silent. An enum makes the compiler ask. That is not theoretical —
the guard in `record.rs` had to grow `every_tool_outcome` for exactly the reason it had to grow
`every_turn_state`, and a new variant now fails to compile rather than slipping past it.

Two things follow that are worth knowing before reading the log:

- **The error text is a tool body**, not a label, so the default retention level withholds it — a failed shell
  command puts its output there, which is precisely the material "conversation only" exists to keep off disk. That a
  call failed, which tool, how long it took, and whether a person stopped it all survive; only the text goes.
- **`is_interrupt` is the only signal anywhere in the hook surface that a person stopped the work.** Nothing
  derives `TurnState::InterruptedUser` from it yet — that is a turn-state decision, not a recording one — but this
  is the evidence it would be derived from, and the plan lists a state nothing currently produces.

**What the envelope now carries.**

`Record` gained `vendor_turn_id` and `vendor_agent_id`, and `turn_started` gained `author`. Three things about that
change are load-bearing rather than incidental, and each is pinned by a test:

- The correlators ride **per observation**, not on the per-connection vendor stamp. A deferred `SessionStart` and the
  prompt that flushes it are written in one batch but did not carry the same correlator — the vendor sends none on a
  session start. One stamp for the batch would have invented that correlation, silently, in a place no existing test
  looked.
- `Record` flattens both the vendor stamp and the event body, which makes **every envelope key a reserved word for
  every event body, now and later**. A collision serialises without complaint and then fails to read back as a
  duplicate field — the line reaches the source of truth and is lost on scan, with no compile error anywhere. This is
  not hypothetical: Claude Code already has a `turn_id` of its own on `MessageDisplay`, with a *different* value from
  `prompt_id` on the same payload. `no_event_body_shadows_an_envelope_key` in `record.rs` is the only guard.
- Retention governs content, not correlation or attribution. `author` sits inside `TurnStarted`, so the compiler
  forces the retention arm to be rewritten — and the tempting fix, `author: None`, would erase it at every level
  including `Full`, on the only path to disk, with every other test still green.

`SegmentLog::append` now takes an `Entry` rather than growing positional arguments, and deliberately has no
`Default`: a future envelope field should break every construction site loudly rather than default itself to absent.

**Four things an adversarial review of that change found, all now fixed and pinned.** They are worth reading as a
list of the shapes this project's bugs take.

- **A daemon restart forked every live conversation.** The registry is derived state and was starting empty, so a
  vendor session that outlived the daemon minted a second `bus_id`. It is now rebuilt from the segments on start —
  which is what "derived state, always rebuildable from the log" was supposed to mean all along. The e2e test whose
  name claimed this property never checked `bus_id`; it does now.
- **A record the daemon could not place vanished without a word.** An orphan — the normal shape of a restart
  mid-turn — was answered `Orphaned` and the hook client treated every non-failure reply as success. The turn's
  remaining events left no record, no defect, and nothing on stderr. The client now reports orphaned and withheld.
- **Resuming skipped lines it could not parse**, so a build reopening a log a newer build extended resumed at a
  sequence number already on disk. `last_sequence` now parses a seq-only envelope: a sequence is a sequence whatever
  the event turns out to be. (`SegmentLog::scan` separately reports a record whose `v` is newer than this build,
  rather than reading it as though it understood it.)
- **The flatten guard could not see the collisions that matter.** It built every event variant with its optional
  fields `None`, and an absent field emits no key — so it could not catch a shadowed *optional* key, which is the
  shape of every field this schema is likely to grow. It also matched on `Event` only, never on the `TurnState` that
  `TurnEnded` flattens. Both fixed; verified by injecting an optional `bus_id` field into `TurnStarted` in a scratch
  clone and watching it fail with `duplicate field`.

**What the real-traffic probe changed.**

The adapter was first built from field names read out of the binary's strings. One real session proved that wrong:
`StopFailure` sends `error`, not `failure_reason`, so the vendor's reason was being replaced by `"unspecified"` while
every test passed. That is fixed, and four real payloads now live in
[`crates/conbus/tests/fixtures/`](crates/conbus/tests/fixtures/) with tests that replay them through the real binary.
Take the lesson generally: **the adapter is only as good as the traffic it has been replayed against.** `Stop` and
`PostToolUse` still have no captured traffic — the probe session failed authentication before reaching either.

**Still open.**

- **`turn_state` has a fourth variant.** The plan lists `complete`, `interrupted_quota`, `interrupted_user`. Since
  `error` has eleven values, collapsing account and request faults into `interrupted_quota` would make the failover
  counter lie, so there is now `interrupted_policy`. Both interrupted variants keep the vendor string verbatim.
  Nothing currently produces `interrupted_user` — Claude Code does not appear to distinguish it.
- ~~**`PermissionRequest` is not mirrored**~~ **Mirrored 2026-09-06** as `permission_asked`.
  `PermissionDenied` still covers only auto-mode classifier denials. The ask carries no
  `tool_use_id` (the emitter passes the id to the hook runner but not into the payload), so
  it joins by tool name plus adjacency rather than by key. An ordinary no is this line
  with no following `tool_use`.
- **Still dropped from payloads already parsed**: `agent_type` and `transcript_path` (both on the common base of
  every payload), `session_title`, `model`, `permission_mode`, `effort.level`.
- **`SubagentStop` is mirrored now**, as a `turn_ended` carrying the subagent's `agent_id` and its closing text.
  `SubagentStart` is not: it carries no prompt, so mirroring it would write a turn that began with nothing. The
  asymmetry is the vendor's.
- ~~**`Notification` is not mirrored**~~ **Mirrored 2026-09-06.** `kind` is the vendor's
  `notification_type`, kept verbatim. The quota-lifted spelling is still unpinned; until a
  capture names it the registration has no matcher and the router does not branch on it.
- **`MessageDisplay` is not mirrored**, and it is the only official channel carrying every assistant message rather
  than the last one. High volume, and it collides with the "conversation only" shipping default, so it is a retention
  decision before it is an adapter change.
- ~~No capture has ever contained a `PostToolUse`; that is the most valuable one still missing.~~ **Installing it
  fixed that.** Live traffic has now confirmed `PreToolUse`, `PostToolUse` and `PostToolBatch`, and with them
  `tool_use_id`, `duration_ms`, `tool_calls` and `prompt_id` — all four had been read off the schema and the
  emit sites and never seen on the wire.
- **Still unverified against traffic**: `agent_id` (verified 2026-09-02: 2,263 live records carried it, `SubagentStop` mirrored),
  `PostToolUseFailure`, `PermissionDenied`, and `StopFailure`. The `agent_id` one carries the most risk: the
  schema says a subagent's hooks arrive under the *parent's* `session_id`, and if that is wrong in the other
  direction — subagents getting their own — the recorder has a bogus-session problem instead, which is worse than
  interleaving. `StopFailure` only arrives by actually exhausting a limit, so the first exhaustion is a recording
  opportunity rather than an interruption.
- ~~`Registry::restore` reads the whole log at daemon start, and that will hurt once backfill lands.~~ **Measured
  after backfill and it does not.** 23,286 records across 78 MB and ten segments: a kill-and-restart is served
  again in under 10 ms, which is below what the probe could resolve. Note the correct way to measure it —
  `connect()` succeeding proves nothing, because `serve` binds the socket *before* it scans, so the kernel
  accepts into the backlog while the daemon is still reading. Time a full request/response round trip instead.
  That ordering is also why the restore window loses nothing: a hook that arrives mid-scan waits rather than
  failing. The derived SQLite index stays deferred until a measurement says otherwise.
- **An older `v` is still read as if it were current.** `scan` rejects a record from a *newer* build; it has no
  opinion about an older one. That is consistent with the bump rule written at `SCHEMA_VERSION` — a bump means an
  older reader would be wrong, not a newer one — but it is an assumption, not an enforcement.
- **The counters have to be written down with the fields.** Recording `author` without deciding how it is counted is
  the worst of both worlds. Routing hops and re-explanation savings must be filtered to human turns or a `/loop`
  firing every five minutes manufactures a passing kill criterion; injected-token cost must count all turns, because
  that is real money and machine turns are where the volume is; and absent `author` must be its own bucket in every
  one of them, never folded into human. The plan's kill criteria (its "킬 크리테리아" section) have no notion of who
  authored a turn and need this amendment.

Success for this milestone is narrow: every turn from both harnesses lands in the log, the log survives a daemon
restart, and installed history is queryable. Routing, packs, and hops come after.

## What to measure once hops work

The kill criteria are numeric and they are in the plan. The one that matters most: **routing hops per week, counted separately from failover hops.** Counting them together hands the verdict to how often the user hits a limit, which is outside our control. And injected tokens are a **cost** metric, not a success metric — under dual-warm they converge to zero by design.

## Two things worth knowing about the competition

Two adjacent products shipped parts of this plan and both have roughly zero users, so they are design references rather than near-term threats. One is a hook-based recorder that already built our stage 0 and never built a router. The other is a project knowledge graph whose own marketing explicitly cedes our layer: "Continuity is not a transcript… the live conversation buffer is your framework's." The real market clocks are Paseo's provider-continuity PR and vendor importers — Codex `/import` already pulls Claude Code sessions.

Their write-back problem is our opening. They need a busy agent to volunteer facts mid-task. We have the whole log, so we derive facts in the background instead. That only works because the recorder exists, which is why the recorder is the first commit.
