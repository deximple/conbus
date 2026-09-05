# Fact schema

Stage 0 contract. Types, lifecycle, evidence, clocks, anchors, provenance,
and how facts are derived. The arena that writes them is stage 1
(`workspace-arena`). This file is the agreement; it is not an implementation.

Facts are derived from the log. A busy agent does not write them. Tool events
alone do not write them.

## Why the log, not the agent

The hard half of a knowledge graph is write-back. The moment an agent knows
something worth recording is the moment it is busy with something else. Graphs
that depend on that goodwill get noisy.

The bus already has the whole record. A cheap model — the same class as the
rolling summary, and never the protected leg — reads N turns, proposes
candidate facts, stamps the source turn, and waits for a person to confirm or
for a workspace policy to auto-accept. That is a continuous backfill, and it
only works because the recorder exists.

## A fact

```text
id            stable, minted when the candidate is accepted
type          one of the types below
claim         one sentence, present tense, no hedging
lifecycle     chosen at write time (required)
supersedes    optional; requires evidence
contradicts   optional; does not pick a winner
fresh_at      when the claim last matched the world
verified_at   when a person or policy last accepted it
anchors       files / symbols the claim is about
provenance    harness, model, bus_id, seq of the source turn
version       monotonic; a supersession is a new version, not an edit
```

## Types

A closed list until the arena has a reason to grow it.

| Type | Holds |
|---|---|
| `decision` | A choice that later turns should not re-argue |
| `constraint` | A rule the workspace is under (legal, vendor, mechanical) |
| `state` | How something is, right now |
| `pointer` | Where the live answer lives (path, symbol, command) |
| `open` | A question the log has not answered |

Unknown types are skipped, the same way an unknown backfill schema is skipped.

## Lifecycle — mandatory at write

One of four. Absence is invalid; "figure it out later" is how a graph rots.

| Lifecycle | Ends when |
|---|---|
| `until_task` | The conversation that produced it ends |
| `until_date` | A calendar day (inclusive) |
| `until_deadline` | A named milestone lands in the log |
| `permanent` | A later fact supersedes it |

`until_deadline` names the milestone; it does not store a guess at the date.

## Relations

**`supersedes` requires evidence.** The evidence is one verbatim line from the
log that shows the old state actually ended — a `turn_ended`, a `routed`, a
commit message the recorder already has. A citation with no line is not
evidence. Without it the old fact stays current.

**`contradicts` keeps both.** The arena does not pick a winner. Both facts stay
visible, with their own provenance, so the next turn sees the conflict instead
of a silently resolved one. Resolving a contradiction is a new `decision` that
supersedes one side, with evidence.

A fact may supersede and contradict different ids. It may not do both to the
same id.

## Two clocks

| Clock | Meaning |
|---|---|
| `fresh_at` | Last time the claim still matched what the log showed |
| `verified_at` | Last time a person or an auto-accept policy agreed |

They move independently. A fact can be fresh and unverified (a new candidate)
or stale and verified (accepted last week, the world moved). The briefing
shows stale-verified facts as a question, not as a given.

Neither clock is `ts` on the source record. That stamp is provenance.

## Anchors

A fact that cannot point at the tree is a remark, not a fact.

- A file path relative to the workspace, or
- A symbol (`crate::route::decide`), or
- Both.

Anchors are not exclusive. Two facts may share one file. A fact with no
anchor is rejected at accept time, except `open`, which may point at a
conversation (`bus_id`) instead.

## Provenance

Every accepted fact names:

- `harness` — which vendor produced the source turn
- `model` — when the vendor said; absent is normal
- `bus_id` — which conversation
- `seq` — which record
- `backend` — `extractive` or the opposite-leg model that proposed it

The digest of a `summarised` record is not provenance by itself. The turns it
covered are.

## Version history

Facts are append-only. An accepted candidate mints `id` at version 1. A
supersession writes version N+1 with a new `claim` and a pointer at N. The
old row is not rewritten. Readers that want "the current graph" take the
highest version per `id` whose lifecycle has not ended.

## What the live path does not do

- The hook does not propose facts. Proposal is a background pass.
- The hop path does not call a model to extract facts.
- The protected leg is never the proposing backend.
- Tool bodies withheld by retention cannot be evidence. If the only proof
  lived in a withheld body, the candidate is dropped, not paraphrased.

## Sync

The local schema is the source of truth. Export adapters come later; the
first candidate is a hosted MCP. Stage 0 does not depend on it.

Consent is a separate contract: a private conversation does not leak into a
linked workspace because a fact named both. That gate is stage 1; shipping
it is a Team-stage decision.
