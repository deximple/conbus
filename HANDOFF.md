# Handoff — 2026-08-30

Written for whoever picks this up next. The design phase is finished and the recorder is started. Read this, then read the plan.

## Where things are

- Repo: `/Applications/conbus`, remote `deximple/conbus`, **private**, branch `main`.
- The plan is the single source of truth: [`.cursor/plans/shared_workspace_context_3f7139b8.plan.md`](.cursor/plans/shared_workspace_context_3f7139b8.plan.md). The filename is a generated ID and no longer matches the content; leave it alone so the Cursor plan UI keeps its association. A reader-facing copy under `docs/` gets generated when the repo goes public.
- `cargo 1.94.1` and `rustc 1.94.1` are installed on this machine, so edition 2024 is available.
- A cargo workspace exists: `crates/conbus` (the binary) and `crates/conbus-core` (the lib). Edition 2024.
  123 tests, `cargo clippy --all-targets -- -D warnings` clean. See "Next task" for what is built and what is not.

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
8. **Implementation:** one binary with argv dispatch (`conbus daemon|hook|log|doctor`), cargo workspace internally. JSONL segments are the source of truth, SQLite is a **disposable** derived index that can always be rebuilt from segments. Data lives in `~/.conbus/`, respecting `$XDG_STATE_HOME`.

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
- Codex hooks need **absolute** node paths because VS Code's app-server has a different PATH. `Stop` hooks need `async: true`.
- Codex quota lives on `account/rateLimits` via app-server, and there is no `StopFailure` equivalent in Codex hooks. Rate limits are **account-scoped**, so a sidecar `codex app-server` polling `account/rateLimits/read` sees consumption regardless of which process burned it. **This is probe #1** — if it doesn't hold, stage 0 is one-directional failover only and the docs must say so.
- Global hooks will mirror our own background `-p` sessions. Mark them with an env variable at spawn and have the hook exit immediately when it sees the marker.

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

`recorder-first`. One week, **recording only, no injection.** Injection hardening is a separate item because the
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
   exit from `UserPromptSubmit` blocks the user's prompt. A dropped record goes to stderr. The `CONBUS_INTERNAL_SESSION`
   marker makes it exit before reading anything, so our own background sessions are not mirrored.
   Also `conbus log` and `conbus doctor`.

`bus_id` minting follows the rule: `SessionStart` only registers a candidate, the first `UserPromptSubmit` mints and
binds, and the remembered session start is then flushed into the log *behind* the prompt that proved the session real.
A prompt mints on its own if the session start never arrived, so Desktop's `/clear` does not cost a conversation.

**Not done, and next.**

5. Backfill from Codex `thread/list` / `thread/read` and OpenCode `export`. Claude needs the fixture-protected parser
   because its official read paths are JS/Python SDK only.
6. **The Codex adapter.** Only `claude_code` is wired. The event model and the wire protocol are already
   harness-agnostic, so this is an adapter plus a hook registration, not a redesign.
7. **More real traffic.** Four payloads are captured; `Stop` and `PostToolUse` are not among them, and neither is a
   real quota exhaustion. The quota fixture only arrives by actually hitting the limit, so treat the first dogfooding
   exhaustion as a recording opportunity rather than an interruption.

**Every tool request now has an answer in the log.**

The tool surface is mirrored end to end: `PreToolUse` → `tool_requested`, and then exactly one of
`PostToolUse` → `tool_use` (succeeded), `PostToolUseFailure` → `tool_use` (failed), or `PermissionDenied` →
`tool_denied`. So an unresolved `tool_requested` now means something specific — the call was still in flight when
the session ended, or the daemon missed the event — rather than "we do not mirror the answer".

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
- **Still dropped from payloads we already parse**: `SessionStart.session_title` and `.model`, `permission_mode`,
  and `effort.level`. Each is a field on a `Payload` we already deserialise.
  (`tool_use_id` and `duration_ms` are recorded now, both verified at the emit site and not only in the schema —
  the object literal carries them, unlike `UserPromptSubmit.source`, whose spread is compiled out. But do not
  oversell `tool_use_id`: nothing joins on it yet, because the log holds one record per *successful* call and that
  record already carries its own input and output. It becomes a join key when `PreToolUse` and
  `PostToolUseFailure` are mirrored.)

- **`PostToolBatch` is not mirrored.** It carries the whole batch's `tool_calls`, which is the vendor's own
  statement of what was issued together — cheaper than inferring batches from adjacency. Not needed for the join,
  but it is the remaining tool-surface event.
- **`MessageDisplay` is not mirrored**, and it is the only official channel carrying every assistant message rather
  than the last one. High volume, and it collides with the "conversation only" shipping default, so it is a retention
  decision before it is an adapter change.
- **Only `claude_code` is wired.** The event model and wire protocol are already harness-agnostic.
- **`agent_id` is recorded but unverified against traffic.** No capture has exercised a subagent yet. The schema says
  a subagent's hooks arrive under the parent's `session_id` with `agent_id` set; if that is wrong in the other
  direction — subagents getting their own `session_id` — the recorder has a bogus-session problem instead, which is
  worse. Same for `tool_use_id`, `duration_ms`, `tool_input`, `tool_response` and `error_details`: read off the
  schema and the emit sites, but **no capture has ever contained a `PostToolUse`** — neither probe session got past
  authentication. That is the single most valuable capture still missing.
- **`Registry::restore` reads the whole log at daemon start.** Correct, and cheap while the log is small. Once
  backfill lands it is a full scan on every start, and that is what the derived SQLite index is for.
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
