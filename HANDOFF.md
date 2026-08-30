# Handoff — 2026-08-30

Written for whoever picks this up next. The design phase is finished and nothing has been built yet. Read this, then read the plan.

## Where things are

- Repo: `/Applications/conbus`, remote `deximple/conbus`, **private**, branch `main`.
- The plan is the single source of truth: [`.cursor/plans/shared_workspace_context_3f7139b8.plan.md`](.cursor/plans/shared_workspace_context_3f7139b8.plan.md). The filename is a generated ID and no longer matches the content; leave it alone so the Cursor plan UI keeps its association. A reader-facing copy under `docs/` gets generated when the repo goes public.
- Two commits. No code, no `Cargo.toml`, no scaffolding.
- `cargo 1.94.1` and `rustc 1.94.1` are installed on this machine, so edition 2024 is available.

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

## Next task

`recorder-first`. One week, **recording only, no injection.** Injection hardening is a separate item because the quirks above take weeks, not days.

Suggested order:

1. Cargo workspace. Two crates to start: the `conbus` binary and a core lib. Don't scaffold crates you don't need yet.
2. Event model and the JSONL segment writer. `schema_version` and a vendor/version stamp on every record from day one, because the sync story depends on the log being shippable rather than mergeable.
3. Unix socket daemon holding derived state in private memory (anonymous mapping, `mlock`, core dumps disabled — not a file-backed mmap ring; that decision was reversed because a plaintext 0600 ring file defeats both the envelope encryption and the capability token).
4. Thin `conbus hook` client, registered against Claude Code first. Identify the calling harness from env/argv.
5. Backfill from Codex `thread/list` / `thread/read` and OpenCode `export`. Claude needs the fixture-protected parser because its official read paths are JS/Python SDK only.

Success for this milestone is narrow: every turn from both harnesses lands in the log, the log survives a daemon restart, and installed history is queryable. Routing, packs, and hops come after.

## What to measure once hops work

The kill criteria are numeric and they are in the plan. The one that matters most: **routing hops per week, counted separately from failover hops.** Counting them together hands the verdict to how often the user hits a limit, which is outside our control. And injected tokens are a **cost** metric, not a success metric — under dual-warm they converge to zero by design.

## Two things worth knowing about the competition

Two adjacent products shipped parts of this plan and both have roughly zero users, so they are design references rather than near-term threats. One is a hook-based recorder that already built our stage 0 and never built a router. The other is a project knowledge graph whose own marketing explicitly cedes our layer: "Continuity is not a transcript… the live conversation buffer is your framework's." The real market clocks are Paseo's provider-continuity PR and vendor importers — Codex `/import` already pulls Claude Code sessions.

Their write-back problem is our opening. They need a busy agent to volunteer facts mid-task. We have the whole log, so we derive facts in the background instead. That only works because the recorder exists, which is why the recorder is the first commit.
