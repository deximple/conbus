# SaaS hypothesis — next work

Date: 2026-09-22. Status: decided 2026-09-23 (Choice 1). Do not implement a host.

Read `AGENTS.md` and `HANDOFF.md` first. This file does not reopen the constraints there. Vendor TUIs stay the window. Tokens stay out of conbus. Prompts are not uploaded.

## Gap

conbus keeps one conversation id while the harness changes, and it is local on purpose. A SaaS that stored the conversation would break that design. No sentence yet says who pays, for which hop, after the design is left as it is.

## Decision to write

Pick one and write it under "Choice" below. Do not leave both open. Do not start a server in this pass.

1. Local tool. There is no SaaS hypothesis. Say that in the README in one sentence. Stop.
2. SaaS. The server may store only `bus_id`, the hop, and the quota fact. It does not store prompt bodies, vendor tokens, or session files. The paid event is one quota hop after which the same `bus_id` still identifies the conversation. Price attaches to that event only.

## Choice

**1 — Local tool.** Written 2026-09-23. The README says the same in one
sentence: "conbus is a local tool. There is no hosted bus, account or price."

Why, in the terms this file set. Option 2 prices one event: a quota hop after
which the same `bus_id` still identifies the conversation. That event has not
happened organically once. Over the 22.5 days of the dogfood window the log
holds zero organic hops, and zero quota-interrupted turns on an attended,
hooked leg; the only organic exhaustions (Codex.app on 2026-09-06 and
2026-09-08, agy on 2026-09-23) happened on surfaces with no hook, where the bus
is a passenger. A price attached to an event with no observed occurrence is not
a hypothesis anyone can test yet. The part of conbus that demonstrably works —
the recorder — is local by design and needs no server.

Reopen when `conbus verdict` scores a window with organic routing hops at or
above the kill criterion. Option 2 above is then the starting text, not a
blank page.

## Done when

"Choice" is either 1 or 2, the README says the same thing, and no host, account, or billing code was added.

## Do not

- Pool quota or credentials across people.
- Parse vendor session files on the live path.
- Pull another project's code into this repo.
- Treat another live-failure writeup as this decision.
