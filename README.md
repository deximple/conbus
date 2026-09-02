# Conbus

A conversation bus for coding agents.

One conversation keeps its identity while the harness underneath it changes. Claude Code hits its limit mid-task, the turn continues in Codex, and nothing gets re-explained. The bus owns the conversation state, the ID, and the routing. The vendor CLI stays exactly as you know it.

Status: recording and briefing. The recorder is built and mirrors Claude Code and Codex sessions into an append-only log — turns, the whole tool surface, and how each turn ended. The briefing is live — a session opening in a checkout is told what happened there, on both harnesses since 2026-09-03 — and there is no routing and no hop yet. The full design, the risk register, and the open decisions live in [the plan](.cursor/plans/shared_workspace_context_3f7139b8.plan.md), which remains the single source of truth. A reader-facing copy gets generated under `docs/` when the repo goes public.

## The shape of it

- Vendor TUIs stay the window. A hook sidecar mirrors every session into an append-only log.
- We mint the conversation ID. Vendor session IDs are aliases of it.
- Routing is the product: per-task first, quota failover second.
- Local first. No hosted dependency, no telemetry, and the bus never reads or stores vendor auth tokens.

## Naming

Conbus is short for conversation bus. Developers tend to read it as context bus, which is also true.
