# Handoff — 2026-08-30

Written for whoever picks this up next. The design phase is finished and the recorder is started. Read this, then read the plan.

## Where things are

- Repo: `/Applications/conbus`, remote `deximple/conbus`, **private**, branch `main`.
- The plan is the single source of truth: [`.cursor/plans/shared_workspace_context_3f7139b8.plan.md`](.cursor/plans/shared_workspace_context_3f7139b8.plan.md). The filename is a generated ID and no longer matches the content; leave it alone so the Cursor plan UI keeps its association. A reader-facing copy under `docs/` gets generated when the repo goes public.
- `cargo 1.94.1` and `rustc 1.94.1` are installed on this machine, so edition 2024 is available.
- A cargo workspace exists: `crates/conbus` (the binary) and `crates/conbus-core` (the lib). Edition 2024.
  67 tests, `cargo clippy --all-targets -- -D warnings` clean. See "Next task" for what is built and what is not.

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
  **This closes the contradiction between the flush barrier and the live-path rule.** The plan's "wait under 2s on
  `Stop` for the last assistant message to land in the transcript" is not needed and should be struck: the vendor
  hands us the text on the payload. The log can hold the conversation without ever opening a transcript.
- **`prompt_id` is on the common base** — "UUID correlating a user prompt with all subsequent events until the next
  prompt", and the same value as the `prompt.id` OpenTelemetry attribute. This is the turn correlator `turn_state`
  and turn atomicity need, and it matters more than it looks because subagents interleave. **Not yet recorded.**
- **`UserPromptSubmit` has a `source`**: `user`, `sdk`, `system`, `loop_wakeup`, `schedule_wakeup`, `poll_event`.
  A machine-injected turn is distinguishable from a human one at the source. That is a better self-exclusion signal
  than our env marker and it is what keeps the survival meter honest. **Not yet recorded.**
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
- **`prompt_id` and `UserPromptSubmit.source` are not recorded yet.** Both are on the payloads today. The first is
  the turn correlator; the second tells a human turn from a machine-injected one. Recording them means one optional
  field on the record envelope, which is a schema change worth making deliberately rather than in passing.
- **Only `claude_code` is wired.** The event model and wire protocol are already harness-agnostic.

Success for this milestone is narrow: every turn from both harnesses lands in the log, the log survives a daemon
restart, and installed history is queryable. Routing, packs, and hops come after.

## What to measure once hops work

The kill criteria are numeric and they are in the plan. The one that matters most: **routing hops per week, counted separately from failover hops.** Counting them together hands the verdict to how often the user hits a limit, which is outside our control. And injected tokens are a **cost** metric, not a success metric — under dual-warm they converge to zero by design.

## Two things worth knowing about the competition

Two adjacent products shipped parts of this plan and both have roughly zero users, so they are design references rather than near-term threats. One is a hook-based recorder that already built our stage 0 and never built a router. The other is a project knowledge graph whose own marketing explicitly cedes our layer: "Continuity is not a transcript… the live conversation buffer is your framework's." The real market clocks are Paseo's provider-continuity PR and vendor importers — Codex `/import` already pulls Claude Code sessions.

Their write-back problem is our opening. They need a busy agent to volunteer facts mid-task. We have the whole log, so we derive facts in the background instead. That only works because the recorder exists, which is why the recorder is the first commit.
