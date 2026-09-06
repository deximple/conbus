# Conbus

A conversation bus for coding agents.

One conversation keeps its identity while the harness underneath it changes. Claude Code hits its limit mid-task, the turn continues in Codex, and nothing gets re-explained. The bus owns the conversation state, the ID, and the routing. The vendor CLI stays exactly as you know it.

Status: recording, briefing, routing, and dual-warm on both legs. The recorder mirrors Claude Code and Codex sessions into an append-only log. A session opening in a checkout is briefed. `conbus hop` decides the next harness from quota and `StopFailure`; `--apply` records the hop and opens or syncs a bus-owned warm on each leg. The vendor TUI stays yours — the bus does not drive a window. Facts: [`docs/fact-schema.md`](docs/fact-schema.md). First-time setup and daily use: [`docs/beginner-manual.md`](docs/beginner-manual.md). The full design lives in [the plan](.cursor/plans/shared_workspace_context_3f7139b8.plan.md).

## The shape of it

- Vendor TUIs stay the window. A hook sidecar mirrors every session into an append-only log.
- We mint the conversation ID. Vendor session IDs are aliases of it.
- Routing is the product: per-task first, quota failover second.
- Local first. No hosted dependency, no telemetry, and the bus never reads or stores vendor auth tokens.

## Naming

Conbus is short for conversation bus. Developers tend to read it as context bus, which is also true.
