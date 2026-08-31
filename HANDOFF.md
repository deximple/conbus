# Handoff — 2026-08-31

Written for whoever picks this up next. The design phase is finished, the recorder is built, and it is installed and
recording on this machine. Read this, then read the plan.

## Where things are

- Repo: `/Applications/conbus`, remote `deximple/conbus`, **private**. The recorder work is on
  `claude/conbus-handoff-docs-f9187a`, 13 commits ahead of `main` and unpushed.
- The plan is the single source of truth: [`.cursor/plans/shared_workspace_context_3f7139b8.plan.md`](.cursor/plans/shared_workspace_context_3f7139b8.plan.md). The filename is a generated ID and no longer matches the content; leave it alone so the Cursor plan UI keeps its association. A reader-facing copy under `docs/` gets generated when the repo goes public.
- `cargo 1.94.1` and `rustc 1.94.1` are installed on this machine, so edition 2024 is available.
- A cargo workspace exists: `crates/conbus` (the binary) and `crates/conbus-core` (the lib). Edition 2024.
  191 tests, `cargo clippy --all-targets -- -D warnings` clean. See "Next task" for what is built and what is not.

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

### The one that cost a session — found by installing it, not by testing it

**A hook that writes nothing to stdout can take the vendor session down.** Registered for real, `conbus hook`
exited zero with zero bytes on stdout, and the client then failed with
`400 messages.983.content.1.text: cache_control cannot be set for empty text blocks`. Reverting the registration
fixed it; re-checking after the revert confirmed it. The recorder was breaking the conversation it exists to
preserve — the single rule this whole client is built around.

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

**It was installed, and is currently reverted.**

Installing it broke the vendor session (see the first quirk below), so the registrations were restored from backup:
both settings files are byte-identical to their pre-install state, no conbus entry remains in either, and the
`launchd` agent is booted out and its plist removed. The corpus and the binary were deliberately kept —
`~/.conbus` holds 23,340 records over 3,426 conversations and `conbus log` reads them without any hook installed.

What installing it proved before it was reverted is below, and stands: the mechanics work. What it also proved is
that they were never verified against a real session, which is how the hook that broke it shipped.

**When it was installed, it was supervised and recording.**

The binary lives at `~/.local/bin/conbus` — outside the worktree, so tidying a branch does not break the hooks.
`conbus install --apply` registered ten events with Claude Code and six with Codex, merging into settings files
that already held fourteen and five of the user's own hook events respectively; every pre-existing key and hook came
back byte-for-byte and still first. A `launchd` agent (`sh.conbus.daemon`, `RunAtLoad` + `KeepAlive`) supervises
the daemon; killing it `-9` gets it restarted, which was checked rather than assumed.

**The corpus is 23,286 backfilled records over 3,425 conversations, spanning 2026-04 to 2026-08, plus live traffic
from the session that installed it.** Bootstrapping the agent requires stopping a manually started daemon first —
otherwise the agent's daemon hits the "already listening" refusal and `KeepAlive` turns that into a restart loop.

One consequence worth knowing, and it is the design working: backfill adopts a session file's own UUID as the
`bus_id`, so when the daemon restores bindings it recognises a *live* session whose history was backfilled and
continues the same conversation. A resumed session's past and present are one `bus_id`, not two.

**Codex is registered and silent.** Its hooks do not run until a human trusts them in Codex itself, and an untrusted
hook fails without printing anything.

**History is recovered.**

`conbus backfill` reads Claude Code's own transcripts — the one place the plan permits a vendor session
file to be parsed, because a backfill failure thins the corpus while a live-path failure loses the
conversation happening now. On this machine it recovered **3,425 conversations, 12,025 turns and 23,280
records from 5.24 GB in four seconds**, into a 78 MB log. It is idempotent: the session file's own name is
a UUID, so it *becomes* the `bus_id`, and a second run recovers nothing rather than a second copy.

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
  backfilled ones predate that and backfill emits no session record at all. Until that is fixed the
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
"~2,500 tokens" stands unconfirmed, so the pack builder is budget-parameterised per leg rather than
assuming the two vendors are symmetric.

One correction to the note above: `<persisted-output>` is the right oracle but the wrong detector.
A hook cannot see the model's context, finding the marker means reading a transcript — banned on the
live path — and ordinary large tool results produce the same marker, so it is not specific. Assert
the budget locally on every emit; use the marker only in a probe against a session we own.

**What a person sees, and how they stop it.** One `systemMessage` line on the turn that was actually
hydrated, naming the source id and the command to inspect it — a field both vendors have, separate
from `additionalContext`, that costs no model tokens. No injection, no line, ever; silence is the
default. `/clear` already means "forget this", so `SessionStart.source == "clear"` suppresses
hydration for that session — a gesture the user already makes rather than one we invented, with the
Desktop caveat above. The explicit off is a command, and a command is acceptable **only** here: a
missed hydration costs one sentence of re-explanation, a wrong one costs credibility in every
session opened afterwards, so the negative case gets a boring lever and the positive case must never
need one.

**Not done, and next.**

5. **Codex backfill.** The Claude side is done; Codex is not, and it is harder than the plan assumes. Three things
   were measured on this machine: `thread/read` returns metadata with `turns: []`, so content needs
   `thread/turns/list` or `thread/items/list` chosen per thread by a `historyMode` field and both gated behind
   `capabilities: {experimentalApi: true}`; `thread/list` sees 360 of the 1,335 rollouts actually on disk, i.e. 27%;
   and the official path has **the same version-skew failure the plan attributes only to file parsing** — 0.147.0
   returned `-32603 unknown variant` reading a thread a 0.150.0-alpha wrote. The plan's reasoning for treating the
   official path as safe and file parsing as risky does not survive that. OpenCode `export` is untouched.
6. **The unmirrored events**, in the order they are worth having: `PermissionRequest` (where an ordinary refusal is
   visible), `Notification` (the only hook-channel signal that a quota limit *lifted*), `MessageDisplay`. Each is
   argued under "Still open". `SubagentStop` was on this list and is mirrored now.
7. **A real quota exhaustion.** The one fixture that cannot be manufactured. Now that the recorder is running, the
   first exhaustion records itself — treat it as an opportunity rather than an interruption.

**Where the milestone stands against its own success criteria.** "Every turn from both harnesses lands in the log":
half. Claude Code is confirmed on live traffic, including the `PostToolUse` this file spent three revisions calling
the most valuable missing capture. Codex is registered and correct as far as the schema goes, but its hooks do not
fire until a human trusts them, so nothing has been seen on the wire. "The log survives a daemon restart":
yes, and now proven at 78 MB rather than argued. "Installed history is queryable": yes — 3,425 conversations back to
April, with `--bus-id`, `--since`, `--tail` and `--grep`.

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
`tool_requested` with no resolution. Neither is `PermissionRequest` mirrored, which is the pre-decision event and
the only place an ordinary refusal is visible at all. So an unresolved request today means: denied by a person,
still in flight when the session ended, or missed by the daemon — and the log cannot yet tell you which.

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
- **`PermissionRequest` is not mirrored**, and it is where an ordinary user refusal is visible — `PermissionDenied`
  covers only auto-mode classifier denials. It fires before the decision and carries no `tool_use_id` (the emitter
  passes the id to the hook runner but not into the payload), so it joins by tool name plus adjacency rather than by
  key. Mirroring it is what would make "every request has an answer" true.
- **Still dropped from payloads already parsed**: `agent_type` and `transcript_path` (both on the common base of
  every payload), `session_title`, `model`, `permission_mode`, `effort.level`.
- **`SubagentStop` is mirrored now**, as a `turn_ended` carrying the subagent's `agent_id` and its closing text.
  `SubagentStart` is not: it carries no prompt, so mirroring it would write a turn that began with nothing. The
  asymmetry is the vendor's.
- **`Notification` is not mirrored**, and it carries the only hook-channel signal that a quota limit has *lifted*.
  For a product whose stated reason to exist is routing on quota, that is the cheapest signal on the surface: the
  hook can be registered with a matcher on the notification type, so the volume cost is near zero.
- **`MessageDisplay` is not mirrored**, and it is the only official channel carrying every assistant message rather
  than the last one. High volume, and it collides with the "conversation only" shipping default, so it is a retention
  decision before it is an adapter change.
- ~~No capture has ever contained a `PostToolUse`; that is the most valuable one still missing.~~ **Installing it
  fixed that.** Live traffic has now confirmed `PreToolUse`, `PostToolUse` and `PostToolBatch`, and with them
  `tool_use_id`, `duration_ms`, `tool_calls` and `prompt_id` — all four had been read off the schema and the
  emit sites and never seen on the wire.
- **Still unverified against traffic**: `agent_id` (no subagent has run under the recorder yet),
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
