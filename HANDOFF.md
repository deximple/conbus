# Handoff

Operator notes for the next agent, and the knowledge this project paid for.

SaaS scope: **Choice 3 — open-core ops ledger, validation first**
(`docs/saas-hypothesis.md`, 2026-09-23; plan `docs/saas-strategy.md`; data
contract `docs/rfd/0002-metadata-sync.md`). No host exists and the product is
still the local tool. The host waits for the gates in the plan — reach,
problem, value, and money that has moved — and for an operator decision; do not
start it in the turn that writes or changes the choice. The reach gate (V0,
the desk count) passed on 2026-10-09: at least six of the first thirty
reachable organisations met the buyer profile. The sync **client**
ships in 0.2.9 (`docs/sync-client-plan.md`, implemented against the drafts);
RFD 0002 and RFD 0003 stay drafts until accepted together, a service still
needs that acceptance first, and no field they do not name may be sent.

Live bus ids, home paths, quota numbers and probe session ids do not belong
here — they live in `HANDOFF.local.md` (gitignored) and in `~/.conbus/`.
Everything below is true of the code, not of one machine, which is why it is
tracked: it took weeks to learn and a clone should not have to learn it again.

## Where things are

- Repo: `deximple/conbus`, **public**. The README and the landing page
  install from a release with `install.sh`, which works for anyone — no `gh`
  sign-in, no access needed. Releasing: `docs/release.md`; what going public
  exposed: `docs/publication-review.md`.
- Design SSOT: `.cursor/plans/shared_workspace_context_3f7139b8.plan.md`.
  The filename is a generated id and no longer matches the content; leave it
  so the Cursor plan UI keeps its association.
- Public name: **conbus.sh**. Landing and waitlist: `site/` (not deployed; `site/README.md`).
- Hook contract: `docs/rfd/0001-hook-contract.md`, pinned by the fixtures in
  `crates/conbus/tests/fixtures/`.
- First-time path: `docs/beginner-manual.md`. Hop walkthrough: `docs/hop-demo.md`.
- Fact schema: `docs/fact-schema.md`. Self-improve loop: `docs/self-improve.md`.
- A cargo workspace: `crates/conbus` (the binary) and `crates/conbus-core`
  (the library), edition 2024, two crates and no more.

## Constraints (do not reopen)

Vendor TUIs stay the window. Never read, store or intermediate vendor auth
tokens — login happens only through the unmodified vendor's own flow. Never
modify vendor binaries. Never write Codex's `trusted_hash`; conbus reads that
record and must never write it, because `hooks/list` hands out the very value
that would forge it. Never pool credentials or quota across people. Never
parse vendor session files on the live path — backfill and gap fill are the
sanctioned exceptions, and only with a fixture-protected parser that skips
what it does not recognise. Never inject into a Codex thread the user opened
(the rule targets `thread/inject_items`; returning `additionalContext` from a
hook is a sanctioned channel). Never send a rolling summary to the leg it is
protecting. Never readdir `~/Downloads`. Do not expose a secret-reading tool
to the model. Do not use vendor trademarks in product or feature names.

**Atomic install.** The binary is written beside its target and renamed, then
`launchctl kickstart -k` — never overwritten in place, for the reason under
"Three live-machine failures" below. Every settings file conbus merges into
goes the same way, through `install::write_atomically`, carrying the mode the
file already had.

**The first start after an upgrade is refused once.** macOS's Background Task
Management records a launch constraint for the agent in
`~/Library/LaunchAgents`, and an ad-hoc-signed binary gets a new identity with
every build. So the first start of a replaced binary exits at once with
`OS_REASON_CODESIGNING | Launch Constraint Violation`, and launchd waits its
`ThrottleInterval` before the next — ten seconds by default, which was the whole
~14 s hook gap of both deploys on 2026-09-23 (`log show` with
`eventMessage CONTAINS "sh.conbus.daemon"` shows it). A `bootout` then
`bootstrap` avoids it: the bootstrap makes Background Task Management look at
the item again ("registerLaunchItem: checking for an updated legacy agent …
updated item"), and the new daemon started 0.3 s after `install --apply`. So
`install --apply` always reloads and never kickstarts; the plist's
`ThrottleInterval` of two seconds is the backstop for any other restart.
From v0.2.6 the binary carries a Developer ID, a stable team identity. Its
first deploy (2026-09-24) started on the first try (`runs = 1`), but that
deploy also reloaded, so whether the signature alone ends the refusal is
still unmeasured. The constraint is not applied to a job
bootstrapped from any other directory, so a probe outside `LaunchAgents` does
not reproduce it.

## Decisions that are closed — and the two that stage 1 reopened

Argued through four review rounds; the reasoning is in the plan.

1. **The bus owns state and id; vendor TUIs stay the window.** Stage 0 does
   not build a chat UI. Building one is what kills the product, because the
   vendor TUI is better than anything we would ship in four weeks.
2. ~~**Stage 0 is Claude Code ↔ Codex only.**~~ **Reopened and reversed in
   stage 1.** Cursor was excluded for having no structured quota signal; it
   now has one (`sessionEnd` / `afterAgentResponse` carry the vendor's own
   limit strings) and is a registered harness, alongside grok and opencode.
3. ~~**Dual-warm is asymmetric — only the Codex leg stays warm.**~~
   **Reopened and reversed in stage 1**, which delivered symmetric dual-warm:
   the Claude warm is a CLI stand-in (`--max-turns 0` plus
   `--permission-prompts none`, auto-deny rather than skip-all) instead of the
   Agent SDK path the original reasoning was avoiding.
4. **Scope is kept by cutting, not by extending.**
5. **The rolling summary cannot be cut**, and must never run on the leg it is
   protecting: `claude -p` draws from the subscription limit, so summarising
   with Claude burns the quota being preserved. Summaries stay extractive;
   hop-path model calls stay zero.
6. **Retention** ships at "conversation only" — tool bodies are not written to
   disk at the default level.
7. **The name is Conbus** — conversation bus. Binary `conbus`, conversation id
   `bus_id`.
8. **One binary with argv dispatch**, JSONL segments as the source of truth,
   SQLite only ever as a disposable derived index. Data lives in `~/.conbus/`,
   respecting `$XDG_STATE_HOME`.

## Known vendor quirks — already paid for, do not rediscover

**Claude Code**

- Hook output strings cap at **10,000 UTF-16 code units**, inclusive, applied
  per field. Past that the content is written to a file and the model gets a
  path plus a short preview. **The failure is silent** — it looks like it
  worked if headers land in the preview. Assert the budget locally on every
  emit; treat `<persisted-output>` in a transcript as a failure, and note that
  it is the right oracle but the wrong detector (a hook cannot read the
  model's context, and ordinary large tool results produce the same marker).
- `SessionStart` fires several times per project within hundreds of
  milliseconds and some are ghosts with no transcript
  (`anthropics/claude-code#78455`). **Mint `bus_id` on the first
  `UserPromptSubmit`, never on `SessionStart`**, which only registers a
  candidate. A prompt mints on its own if the start never arrived, so
  Desktop's `/clear` — which omits `source` (`#76704`) — costs no conversation.
- `SessionStart` sources are `startup`, `resume`, `clear`, `compact`, `fork`.
  Subagents are not a session source; they get `SubagentStart` / `SubagentStop`
  with `agent_id` and `agent_type`.
- The hook reply shape is
  `{"hookSpecificOutput":{"hookEventName":"…","additionalContext":"…"}}`, and
  `hookEventName` **must equal the firing event** or the whole result is
  discarded — non-blocking, reported once per session and then suppressed, so
  a drifted name would show one error and go quiet forever. Carry the event
  through from the payload.
- Exactly three things block the user's prompt: `"continue": false`,
  `"decision": "block"`, and **exit code 2** regardless of what was printed.
  A blocked turn also drops the pack.
- **The pack lands after the user's prompt**, not before it. Recency favours
  our text over the person's own words, so precedence has to be stated inside
  the pack rather than inferred from position.
- `UserPromptSubmit`'s hook timeout is **30 seconds**, not the 10-minute
  global default, and a timed-out hook's partial stdout is parsed by nothing.
- **`StopFailure`'s reason field is `error`**, not `failure_reason` — the
  first adapter used the latter, produced `reason: "unspecified"` on a real
  failure, and looked like it worked. `error` has **eleven** values;
  only `rate_limit` and `overloaded` mean "this leg cannot serve the turn
  right now". `invalid_request`, `model_not_found` and `max_output_tokens` are
  faults routing does not fix, which is why the recorder keeps the vendor's
  string verbatim and `turn_state` has `interrupted_policy` beside
  `interrupted_quota`.
- `Stop`, `StopFailure` and `SubagentStop` carry `last_assistant_message`, so
  the plan's sub-2s flush barrier on `Stop` is not needed — but it is the text
  of the **last** message only, and it collapses to absent when that message
  is pure tool use.
- `prompt_id` is one slot on a process-global request journal, so it is stale
  on session-lifecycle events, and it does **not** de-interleave subagents —
  `agent_id` is the field that does. A subagent's hooks arrive under the
  **parent's** `session_id`.
- `UserPromptSubmit.source` exists in the schema and is **not sent**; the
  transcript's `promptSource` is a *larger* vocabulary (`typed`, `queued`,
  `suggestion_accepted`), and `sdk` is stamped on hand-typed prompts. Neither
  is a self-exclusion signal. Attendance is decided by
  `CLAUDE_CODE_ENTRYPOINT`, and an unknown counts as a person.
- `PermissionDenied` fires only for auto-mode classifier refusals, not for a
  person answering no; `PermissionRequest` is the pre-decision event and
  carries no `tool_use_id`, so it joins by tool name plus adjacency.
- `mlockall` returns ENOSYS on macOS. `doctor` reports `memory_locked=false`
  with the errno rather than claiming a guarantee we do not have.

**Codex**

- Hooks need **absolute** paths because the app-server's PATH differs.
- **The trust gate fails silently**: a registered-but-untrusted hook is never
  spawned and nothing is printed. A person approves inside Codex's own startup
  dialog, which reviews every new or changed hook in every file at once — so
  approving conbus's also approves whatever else sits untrusted beside them,
  and `doctor` says so, because being asked to consent without being told the
  scope is not consent. Two traps follow from the same hash: editing an entry
  flips it to `modified` and it silently stops running, and inserting a hook
  ahead of one shifts its group index and orphans its trust record.
- **Codex fails a hook over fields it does not take**: `PreToolUse` rejects
  `continue`, `stopReason` and `suppressOutput`; `PostToolUse` rejects
  `suppressOutput`. The Claude-shaped silent reply produced "hook (failed)"
  twice per tool call while every record still landed. Codex gets `{}`.
- `additionalContext` is accepted on five of eleven events;
  `UserPromptSubmit` is its only per-turn channel. Its limit is ~2,500 tokens
  where a token is `ceil(utf8_bytes/4)`, so the real inline budget is 10,000
  UTF-8 **bytes** — the same number as Claude's in a different unit.
- `SessionEnd` gets **one second** — Codex's own default, the tightest
  deadline on the whole surface, which is why the hook client bounds its call
  to the daemon at 500ms.
- `SessionStart` fires at the first prompt, not when the TUI opens. A session
  closed without a prompt fires `SessionEnd` alone, which the daemon can only
  orphan. `tool_use_id` is `exec-<uuid>`; `tool_response` is a string.
- Codex spawns a **memory-writing agent** beside a session, with its own
  `session_id`, no transcript, and a 50 KB instruction prompt beginning
  `## Memory Writing Agent`.
- Async hooks were dropped by 0.147.0 and are allowed again from 0.148.
- Quota lives on `account/rateLimits/read` via the app-server and is
  **account-scoped** — probe #1, and it held: a sidecar reads consumption
  without opening a thread, regardless of which process burned it. There is no
  `StopFailure` equivalent in Codex hooks, so a Codex exhaustion is not a log
  event. The live `rateLimitReachedType` is `rate_limit_reached`.
- **Desktop spends quota the hooks never see.** Same account, no hook traffic.
  The bus is a passenger on any surface it did not spawn — the design said so;
  this is the measurement.

**Grok**

- **Grok runs Claude Code's hooks as well as its own.** Its hook docs name
  `.claude/settings.json` and `.cursor/hooks.json` beside `~/.grok/hooks/*.json`,
  and every registration it finds runs. From 2026-09-06, the day `install`
  first wrote Grok's own file, every Grok event reached the log twice: one
  conversation, identical ids and bodies, 30–330 ms apart, consecutive seqs,
  both stamped `grok` because the harness is read from the payload. The hook
  now answers the copy that did not come through `--harness grok` without
  recording it, and only while Grok's own file registers that event through a
  binary that exists (`hook::client::grok_echo`) — without the file, Claude
  Code's registration is the only copy. The twins already written stay, since
  the log is append-only; `conbus usage` counts them once.
- `~/.grok/hooks/*.json` is trusted without asking. Its Claude-compatible
  hooks shipped loaded and never dispatched in 1.0.3
  (`xai-org/plugin-marketplace#236`), which is why Grok's own file is the copy
  kept.

**Both**

- Global hooks mirror our own background sessions unless marked. A hook
  carrying the `CONBUS_INTERNAL_SESSION` marker still reads all of stdin
  first — the vendor never writes into a closed pipe — and is then answered
  with the silent reply and left unrecorded.
- The two vendors cannot share a session id: Claude Code takes
  `--session-id <uuid>`, Codex assigns thread ids and its `ThreadStartParams`
  has no `threadId`. Identity is carried by the registry's alias map and never
  by making the ids equal.
- A command run inside a Codex session cannot learn its own thread id; Claude
  Code sets `CLAUDE_CODE_SESSION_ID` in a child's environment and Codex sets
  no equivalent.

## Three live-machine failures worth not repeating

**A hook that wrote nothing to stdout coincided with a vendor session going
down.** Registered for real, the hook exited zero with zero bytes, and the
client then failed with `400 … cache_control cannot be set for empty text
blocks`. Reverting the registration fixed it. The correlation is solid and the
mechanism is *not* confirmed — a later pass through the vendor bundle could not
find the path that turns a zero-byte stdout into an empty text block. Two things
outlive the fix: **"the hook writes 0 bytes" and "the vendor receives nothing"
are different claims, and only the first was measured**; and **nothing in the
test suite could have caught it**, because the tests drive a real binary against
a real socket and all passed. Every path now prints
`{"continue":true,"suppressOutput":true}`, pinned across all six paths.

**The recorder went silent for two hours and every check said it was healthy.**
The binary had been overwritten in place while the launchd daemon held that
inode mapped; macOS taints the vnode when a signed binary changes under a
mapping, so the kernel killed every exec of that path before `main` ran — exit
137, no output, for `--help` as much as for `hook`. And the drop log's emptiness
proved nothing, because the drop log is written by the hook: a hook that cannot
start writes no drops. `doctor` now runs the registered command through a shell,
marked internal, insists on parseable JSON, prints the most recent record by
time, and compares the binary's build stamp against the running daemon's.

**The log's own repair split two conversations, and counting found it, not
testing.** Every `backfill --apply` runs with the daemon stopped, and every
session open at that moment came back bound to backfill's copy of itself,
because the copy was the newest record and restore took the newest. The suite
was green throughout: `Registry::restore` had one test, with one record. What
found it was a survey of the real log keyed by `(harness, vendor_session_id)`.
Two rules follow. **A reconstruction never outranks what the recorder saw**, in
the registry and in backfill alike. And **run the survey, not the suite, when
the question is about the corpus** — `doctor` now carries that survey as its
`twins` line.

A fourth, smaller: **Darwin refuses to set socket options on a peer that has
already hung up**, with `EINVAL`, and `doctor`'s liveness probe is exactly such
a peer. A peer that left before it was served is nobody to serve.

**A fifth, and both halves were found by counting rather than by reading.** On
2026-09-08 one conversation wrote **146 `routed` records in 38 minutes** — the
daemon answered every `InterruptedQuota` from a leg that kept refusing, spawned
a `hop --apply` behind each, and left 32 headless Claude sessions leased. The
guard already existed (`Chapter.pending_hop`, which `hop --apply` consults) and
the daemon simply did not look at it. The lesson from the twin defect holds:
**run the survey, not the suite, when the question is about the corpus** — 152
of the log's 168 `routed` records are that one runaway, and no test was ever
going to say so.

And **"I connected" is not "somebody is serving"**. Closing a listening AF_UNIX
socket does not tear the path down at once, so a connect to a daemon that has
just died can still succeed — measured under load with `lsof` showing no
process holding the path. `bind` refused the new daemon its own socket on that
evidence, which under launchd's `KeepAlive` turns a crash into a restart loop.
The probe now asks whether anything is *serving*: a daemon that is busy says
nothing, because our request is what it waits for, so the read expiring is the
positive answer; a peer that is going away hands back end-of-file at once.

## What the log holds, and why it holds it that way

- One flat JSON line per record, fixed-width UTC timestamp first so a text
  sort gives append order, `v` on every line. Segments roll at 8 MiB; a line
  torn by a crash is reported as a defect and the next append heals the
  missing newline rather than being glued onto the wreck.
- Retention is applied **inside the writer**, the only path to disk, so no
  caller can write a body the workspace said to withhold.
- `Record` flattens both the vendor stamp and the event body, which makes
  every envelope key a reserved word for every event body, now and later. A
  collision serialises without complaint and fails to read back as a duplicate
  field — the line reaches the source of truth and is lost on scan, with no
  compile error anywhere. `no_event_body_shadows_an_envelope_key` is the only
  guard, and it only works because every field it builds is populated.
- A tool call is two records — `tool_requested` and `tool_use` — because a
  request and its result are two moments and the log is append-only. A denial
  is its own event, not a third outcome, because `tool_use` means the tool
  *ran*.
- Backfill stamps `backfilled_from`, because a reconstructed record is a
  weaker fact than a mirrored one and a reader that cannot tell them apart
  will over-trust the log. Three sources: Claude transcripts, Codex threads
  through the app-server, and the official Claude.ai conversation export.
- The transcript carries three things the hook path does not: the vendor
  version, `promptSource`, and when it happened.

## What a person sees, and how they stop it

One `systemMessage` line on the turn that was briefed, naming the conversation
recounted and the command to inspect it — a field both vendors have, separate
from `additionalContext`, costing no model tokens. No injection, no line, ever:
silence is the default. A session whose flushed start says `source: clear` is
not briefed, because `/clear` already means "forget this". A prompt that opens
with a vendor's own tell that a program wrote it is not briefed even when the
entrypoint calls the session attended. A conversation with both recovered and
live records is described as "partly reconstructed", not "not observed live".
The explicit off is `CONBUS_BRIEFING=off` in the environment the vendor runs
hooks in, carried on the wire so the daemon neither injects nor records a
briefing nobody received.

The pack refuses three things, each for a reason that cost something: **the
fence is not forgeable** (its nonce comes from the id of the session being
told, minted after every word the pack quotes, and quoted text is scrubbed of
it), **it never truncates the assembled string** (the thing at the end is the
closing fence, so cutting the string is how a quoting problem becomes an
injection problem — over budget it drops whole chapters, then refuses), and
**it carries the tail, not the head**.

## Shipped user surface

Stranger path (0.1.0): `install.sh` (download, checksum, place, report) →
`install --apply` → trust the hooks in Codex → `doctor` → `hop --demo` →
`conbus run claude_code`. `conbus uninstall --apply` takes everything back out
and keeps the log; `tests/install_roundtrip.rs` holds that promise, key order
included. Chat orders and `conbus go <leg>` are directed hops. Cursor landing is
`cursor -n`. Releases come from a `vX.Y.Z` tag (`.github/workflows/release.yml`);
CI gates every pull request. Each release is then signed with the Developer
ID and notarized on the operator's Mac (`scripts/sign-release.sh`,
`docs/release.md`). Repo visibility and DNS stay operator actions.

## What the counters mean

`routing_hops` and `failover_hops` are **organic only**: a vendor said it could
not serve the turn, or its own quota reading said the account was spent. A hop
an operator typed (`conbus go`, `hop --to`, a chat order) and a hop a run
declared staged (`CONBUS_PROVENANCE`) are counted in buckets of their own and
printed on an `excluded` line — hiding them would be the same lie as counting
them. Records written before `Event::Routed` carried `provenance` are
unattributed, which is its own bucket and never folded into organic.

A closed 28-day window with no organic hop is **inconclusive**, not a kill:
manufactured hops cannot answer the question the criteria ask, in either
direction. On 2026-09-08 all eleven hops on the machine were manufactured and
the dashboard read them as "routing 4 (3.35/week)" against a criterion of
three.

## Still open

Measured at 1.0 (2026-11-01, a 10 GB real log; `docs/plan-1.0.md`):

- Start to serving is 11 s against a 5 s target: `prepare`'s meta scan is
  5.3 s of it. Hooks in that window spool and are not lost. The next step is a
  restore checkpoint under `cache/` (registry, chapters, split and alias
  summaries, and per-conversation turn ids and prompt digests so gap filling
  stays exact).
- `doctor` is 3.35 s with vendors off PATH against 3 s; the serial briefing
  replay is about 2.3 s.
- The daemon holds about 445 MB at serving.
- Linux: install, the systemd unit and fixture hooks are checked; real vendor
  sessions on Linux are not.
- cursor-agent in print mode fires no prompt or turn-end hook, so it leaves no
  turn records. OpenCode 2.x publishes turn ends, tool calls and tokens that
  the plugin does not forward yet.
- `hook --inference-listen` authenticates nothing; it binds loopback only and
  relies on the terminator running on the same machine.

- `MessageDisplay` is not mirrored — the only official channel carrying every
  assistant message rather than the last one. High volume, and it collides
  with the "conversation only" default, so it is a retention decision first.
- Still dropped from payloads already parsed: `agent_type`,
  `transcript_path`, `session_title`, `model`, `permission_mode`,
  `effort.level`.
- `Notification`'s quota-lifted spelling is unpinned until a capture names it,
  so the registration has no matcher and the router does not branch on it.
- **The first organic hop happened on 2026-09-27 and arrived too late to
  matter.** A Claude desktop session doing background work was stopped by a
  vendor `rate_limit` at 12:25:32Z, and the daemon decided `displaced →
  codex` (`provenance: vendor_signal`). The turn was a `<task-notification>`,
  not a typed prompt. The limit lifted by 12:41, and the app's own "usage
  limit ... has reset now" prompt carried the session on in Claude. The Codex
  warm thread was created at 12:47:08 (its UUIDv7 says so), with the leases at
  12:47:10 and 12:47:25 and `continued` at 12:48:16 — **21.6 minutes after
  the decision**, and unused.
  - **What ruled things out:** the automatic `hop --apply` left no line in
    `daemon.log` then, so the cause is not pinned down. No agent transcript or
    shell history ran `hop`, and the daemon did not restart. Replaying the
    decision on a copy of the log cut at it took 2.0 s.
  - **Candidates:** calls on that path that had no limit (`gh pr view`,
    `git`), and the launchd agent's default resource limits: the daemon runs
    at priority 20, and on 9/27 its start-up took 34 s against 5 s on 9/24.
  - **Fixed since:** `gh` and `git` are bounded to 5 s (`code::LIMIT`), and
    every automatic hop now writes its route and its time to `daemon.log`.
    The next one will say where the minutes go.
- **The second organic hop, on 2026-10-30, did not repeat the wait.** The
  daemon's own line — `automatic hop --apply for <bus> in 67571 ms: hop
  displaced claude_code → codex (current turn hit quota)` — says 67.6 s end
  to end: far better than 21.6 minutes, and still slower than a hop should
  be; where those seconds go is not yet measured.
- **42 Cursor conversations were lost before 2026-09-09** and are not
  recoverable: their tool events were dropped as orphans because no
  `beforeSubmitPrompt` ever arrived for them. Letting a Cursor tool call mint
  (2026-09-09) stopped the orphans and created a worse defect: worker-shaped
  sessions send their tool hooks with an **empty** conversation id, and
  `(cursor, "")` became one conversation — 877 tool records from two
  sessions when the 4th audit found it, 4,569 by the time it stopped growing
  on 2026-09-13. 0.1.0 refuses an empty id on both sides of the socket
  (`hook::NO_SESSION`) and doctor names the merged conversation (`nameless`).
  Those payloads are dropped now, visibly. Which field, if any, names the
  session in a worker payload is still unread — capture one rather than
  guessing.
- **Duplicates the Grok echo fix does not cover.** Cursor: some
  `turn_started`/`turn_ended` pairs land milliseconds apart (18 + 18 in the
  week to 2026-09-23), most likely the same double registration, since Cursor
  runs Claude Code's `UserPromptSubmit`/`Stop` hooks beside its own
  `hooks.json`. Do not copy Grok's guard onto it: Cursor's two files "do not
  always both fire", and the copy stood down could be the only prompt that
  mints. Find out which copy ever arrives alone first. `afterAgentResponse`
  and `stop` both write `turn_ended` on purpose; since 0.2.4 `usage` counts a
  Cursor turn's several stops once, its most serious, and reads a limit from
  the response text only when that text is Cursor's one-line error.
  About 200 `tool_use` records a week share a `tool_use_id` without
  matching; only `postToolUse` writes `tool_use`, and it comes only through
  Claude Code's registration, so they are not echoes. OpenCode: **fixed in
  the plugin (0.2.4); a machine gets it from `install --apply` and an OpenCode
  restart.** The 0.2.3 plugin sent a `turn_started` on
  every `message.updated` of a person's message, with `summary.title` as the
  prompt — OpenCode's summariser publishes that message again after every
  agent step (`anomalyco/opencode#43551`) — and forwarded every text part
  without asking whose it was, so the agent's replies and OpenCode's own
  `synthetic` text were recorded as prompts: 61 `turn_started` for about 8
  prompts in the week to 2026-09-23. It could also take OpenCode down: a hook
  that closed its stdin unread raised an uncaught `EPIPE` in OpenCode's own
  process. The plugin now learns whose a message is from `message.updated`
  and sends a person's message once, from its first text part that is neither
  `synthetic` nor `ignored` (OpenCode's own timeline rule); a part that comes
  before its message waits for it, bounded. Checked against OpenCode's source
  at 1.18.31, the version on this machine: `event` hooks are called in
  publication order and never awaited, and a message is published before its
  parts. What remains: the records already in the log stay — a `msg_…` id
  names one message, so a second `turn_started` for it is always a re-report
  and can be counted once — and a machine keeps the 0.2.3 plugin until
  `conbus install --apply`, which now replaces a plugin this build did not
  write instead of calling it registered. Two paths still put a person's
  earlier words under a new id: an overflow compaction replays the last
  prompt as a new message, and `session.fork` copies the history into the
  new session. Not read: whether the first message of a `task` child
  session, which the parent agent writes, reaches the plugin as a person's
  text part.
- Nothing produces `interrupted_user`; `is_interrupt` on a failed tool call is
  the only signal anywhere that a person stopped the work.
- An older `v` is still read as if it were current. `scan` rejects a record
  from a *newer* build and has no opinion about an older one.
- **The counters have to be written down with the fields.** Routing hops and
  re-explanation savings must be filtered to human turns or a `/loop` firing
  every five minutes manufactures a passing kill criterion; injected tokens
  must count all turns, because that is real money; and absent `author` is its
  own bucket in every one of them, never folded into human.
- Kill criteria: **routing hops per week, counted separately from failover
  hops.** Counting them together hands the verdict to how often the user hits
  a limit, which is outside our control. Injected tokens are a **cost**
  metric, not a success metric.

## Next task

1.0 is cut from `docs/plan-1.0.md`; its progress table at the end says what
landed, what was measured and what was cut. What is left needs a person:

1. **Canary.** Run `scripts/canary.sh preflight`, then `start`, on this Mac
   with the release candidate, outside the blackout hours, and leave it seven
   days; `status` and `verdict` read it, `rollback` puts 0.2.x back. `v1.0.0`
   is tagged only on a passing verdict (`docs/plan-1.0.md` §7).
2. **RFD 0002 and 0003** stay drafts until the operator accepts them; no sync
   service exists.
3. Then back to `docs/self-improve.md`, from live evidence rather than this
   file's memory.

The narrative of the 0.2.x releases that used to live here (deploys, the first
organic hop, the SaaS weeks) is in this file's git history and in
`CHANGELOG.md`.
