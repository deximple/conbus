# Self-improve loop

The next agent runs this. Do not skip observation and call the product done.

## Cycle

1. **Observe** — `conbus doctor`, today's `~/.conbus/drops.jsonl`, `conbus log --tail 5`, `git status`, `conbus --version` vs `daemon.build`.
2. **Pick one** — the most expensive *live* defect (still arriving), not the largest historical pile. Write one sentence: goal, evidence, success check.
3. **Change one thing** — surgical. Vendor tokens, vendor binaries, `~/Downloads` readdir, and in-place overwrite of the installed inode stay forbidden.
4. **Verify** — the failure condition, then the nearest regression. Hook failures are silent; prefer a live vendor event or a fixture that is a real payload.
5. **Record** — append a dated line under Ledger below. Update `HANDOFF.md` Next task to the next live defect, or “observe again”.
6. **Repeat** — a new cycle starts from current evidence, not from this file's memory.

Stop a cycle when the picked defect is verified fixed. Do not mark the product finished because the cycle stopped.

## Live signals that are not defects

- `route stay on cursor` without a classified quota code.
- Chat Desktop hooks off. Official path is Privacy Export / Enterprise Inference.
- Twin sessions bound to their live conversation.
- Split sessions that already carry `aliased`.

## Ledger

- 2026-09-07 cycle 1 — live defect was today's Broken pipe / empty-reply EOF (59 drops by 06:01Z, last EPIPE 06:00:24Z). Root cause: the poll listener is non-blocking; accepted fds inherited that, so a hook accepted before it wrote was dropped as silent. Fix: `set_nonblocking(false)` on accept; listen backlog 256 at `bind`; drain the accept queue; retry write-failures after 40ms; count empty EOF as transient and do not resend it. Verified: `a_burst_of_hooks_does_not_lose_records_to_a_full_backlog` + `a_silent_connection_does_not_hold_the_hooks_behind_it`; installed `147a377ec+dirty` pid 79456; 0 drops after 06:01:40Z; this Cursor bus `01a074f4-…` still appending (seq 53322+).
- 2026-09-09 cycle 2 — audit sweep, eight items. Live defects picked from the
  2026-09-08 audit rather than from this file's memory. **The gate first**: the
  suite was not flaky, it was *hanging* — three modules kept separate locks over
  the same process-global `CONBUS_CLAUDE_BIN` / `CONBUS_CODEX_BIN`, and a test
  whose variable was not seen fell back to the default name and spawned the real
  `claude` and the real `codex app-server`; ten of those were still running after
  forty minutes, holding the build lock. Rust 2024 marks `set_var` unsafe for
  exactly this reason, so the fix was injection at the edge, not a bigger lock;
  nothing in the crate mutates the environment now. Then: 42 Cursor conversations
  were being dropped whole (4,229 orphan tool events since 09-05) because no
  `beforeSubmitPrompt` arrived to mint them — a Cursor tool call mints now. The
  daemon stamps every line it writes, hands a landing to a thread instead of
  waiting on `cursor -n`, and bounds the Inference socket (which surfaced a
  second defect: an unread frame was answering with Claude's hook control JSON
  instead of a verdict). Settings files are replaced by rename. A contested hop
  seat now seats nobody rather than the last writer; stale StatusLine readings
  expire; a program's hop stays or waits instead of changing vendor; the hop is
  durable before a vendor thread is opened and the duplicate unchecked lease is
  gone. And `Event::Routed` carries who decided it, so `verdict` counts organic
  hops only — all eleven on this machine were typed or staged, and it read them
  as "routing 4 (3.35/week)" against a criterion of three. 521 tests, five
  consecutive identical runs, clippy and fmt clean, from 488-489 with one or two
  failing differently each run. Next live defect: whether the orphan drops with a
  `preToolUse` detail actually stop, and the first organic hop — which cannot be
  manufactured, and which `verdict` now refuses to score a window without.
