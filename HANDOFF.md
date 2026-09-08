# Handoff

Operator notes for the next agent. Live bus ids, home paths, and quota
numbers do not belong here — they live in `HANDOFF.local.md` (gitignored)
and in `~/.conbus/`.

## Where things are

- Repo: `deximple/conbus`, **private** until an operator flips visibility.
- Design SSOT: `.cursor/plans/shared_workspace_context_3f7139b8.plan.md`.
- Public name: **conbus.sh**. Landing: `docs/www/index.html`.
- Hook contract: `docs/rfd/0001-hook-contract.md`.
- First-time path: `docs/beginner-manual.md`. Hop walkthrough: `docs/hop-demo.md`.
- Self-improve: `docs/self-improve.md`.

## Constraints (do not reopen)

Vendor TUIs stay the window. Never read vendor auth tokens. Never write
`trusted_hash`. Never inject into a thread the person opened. Never send
a rolling summary to the leg it is protecting. Never readdir `~/Downloads`.
Atomic install only: write beside, `mv`, then `launchctl kickstart -k`.

## Shipped user surface

Stranger path: clone → build → `install --apply` → `doctor` → `hop --demo`
→ `conbus run claude_code`. Chat orders and `conbus go <leg>` are directed
hops. Cursor landing is `cursor -n`. Repo visibility and DNS stay operator
actions.

## Next task

Run `docs/self-improve.md` from current evidence. Score the dogfood window
with `conbus verdict` — do not declare the 4-week clock closed unless the
command says it is. A model digest is `conbus summary --bus-id`; hop-path
model calls stay zero.
