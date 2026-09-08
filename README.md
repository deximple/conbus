# Conbus

A conversation bus for coding agents.

One conversation keeps its identity while the harness underneath it changes. Claude Code hits its limit mid-task, the turn continues in Codex, and nothing gets re-explained. The bus owns the conversation state, the ID, and the routing. The vendor CLI stays exactly as you know it.

Status: recording, briefing, routing, and dual-warm on Claude and Codex. A session opening in a checkout is briefed. Quota or `StopFailure` hops automatically. A chat line that is only an order (`이제 커서로 작업할거야`) or `conbus go cursor` is a directed hop. Cursor opens the checkout with official `cursor -n`. The vendor TUI stays yours. Facts: [`docs/fact-schema.md`](docs/fact-schema.md). First-time setup: [`docs/beginner-manual.md`](docs/beginner-manual.md). Design: [the plan](.cursor/plans/shared_workspace_context_3f7139b8.plan.md).

## The shape of it

- Vendor TUIs stay the window. A hook sidecar mirrors every session into an append-only log.
- We mint the conversation ID. Vendor session IDs are aliases of it.
- Routing is the product: per-task first, quota failover second.
- Local first. No hosted dependency, no telemetry, and the bus never reads or stores vendor auth tokens.

## Naming

Conbus is short for conversation bus. Developers tend to read it as context bus, which is also true. Public name: [conbus.sh](docs/www/index.html). License: [Apache-2.0](LICENSE).

## Install (stranger path)

macOS, with `claude` and `codex` already logged in through those binaries.

```bash
git clone https://github.com/deximple/conbus
cd conbus
cargo build --release -p conbus
./target/release/conbus install --apply
conbus doctor
conbus hop --demo
```

`install --apply` places `~/.local/bin/conbus` (beside, then `mv`), merges hooks, and writes `~/Library/LaunchAgents/sh.conbus.daemon.plist`. It does not write Codex `trusted_hash`. Trust the hooks inside Codex, then `conbus run claude_code`.

Daily: `conbus run claude_code`, or type a hop order in the vendor chat. Preview: `conbus hop cursor`. Apply: `conbus go cursor`.

Hop walkthrough: [`docs/hop-demo.md`](docs/hop-demo.md). Hook contract: [`docs/rfd/0001-hook-contract.md`](docs/rfd/0001-hook-contract.md). Four-week score: `conbus verdict` (an open window is not a kill). Opposite-leg digest: `conbus summary --bus-id`.
