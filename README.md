# Conbus

A conversation bus for coding agents.

One conversation keeps its identity while the harness underneath it changes. Claude Code hits its limit mid-task, the turn continues in Codex, and nothing gets re-explained. The bus owns the conversation state, the ID, and the routing. The vendor CLI stays exactly as you know it.

Status: design. No code yet. See [PLAN.md](PLAN.md) for the full design, the risk register, and what is still undecided.

## The shape of it

- Vendor TUIs stay the window. A hook sidecar mirrors every session into an append-only log.
- We mint the conversation ID. Vendor session IDs are aliases of it.
- Routing is the product: per-task first, quota failover second.
- Local first. No hosted dependency, no telemetry, and the bus never reads or stores vendor auth tokens.

## Naming

Conbus is short for conversation bus. Developers tend to read it as context bus, which is also true.
