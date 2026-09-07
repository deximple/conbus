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
