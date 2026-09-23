# SaaS hypothesis

Date: 2026-09-22. Status: **Choice 3, written 2026-09-23** and revised the same
day, replacing Choice 1. No host exists, and this file starts none.

Read `AGENTS.md` and `HANDOFF.md` first. This file does not reopen the constraints there. Vendor TUIs stay the window. Tokens stay out of conbus. Prompts are not uploaded.

## Gap

conbus keeps one conversation id while the harness changes, and it is local on purpose. A SaaS that stored the conversation would break that design. No sentence yet says who pays, for which hop, after the design is left as it is.

## Options

1. **Local tool.** There is no SaaS hypothesis. Say that in the README in one sentence. Stop.
2. **SaaS on the hop.** The server may store only `bus_id`, the hop, and the quota fact. It does not store prompt bodies, vendor tokens, or session files. The paid event is one quota hop after which the same `bus_id` still identifies the conversation. Price attaches to that event only.
3. **Open-core ops ledger.** The recorder stays local and free. A paid layer
   receives counts and nothing else — the rows `conbus usage --shareable`
   prints, under `docs/rfd/0002-metadata-sync.md` — and shows a team where its
   agents stop, fail, or wait on a person, across vendors. It is delivered as a
   hosted page or as a feed into the monitoring the team already runs; the
   problem interviews decide which. The paid event is a team acting on that.

## Choice

**3 — Open-core ops ledger, validation first.** Written 2026-09-23, revised the
same day after an adversarial review.

**Why it moved from 1.** Choice 1 answered whether the hop could be sold, and
that answer stands: Option 2 prices an event that has not happened organically
once, so it stays closed. What changed is the question. The operator asked on
2026-09-23 what else the recorder is. Three landscape surveys and a review of
them (summarised in `docs/saas-strategy.md`) found that most of it is done
elsewhere:

- Capture, memory, briefing and handoff are common, mostly free, and being
  absorbed by the platforms.
- Vendors document their own stops — Claude Code's hooks name `rate_limit`,
  `billing_error` and `account_on_hold` — and ship admin views below the top
  plans too: Claude Team analytics, the Claude Code Analytics API for any
  Console organisation, Copilot's organisation dashboard, Cursor's Admin API.
- One product already sells a cross-vendor usage view by person, by capturing
  prompts and commands through hooks it deploys with device management.

What is left is narrower: a view of where agents stop, fail and wait on a
person, **merged across vendors and built from counts alone**. The recorder
already files stops by cause across five harnesses and knows whether a person
was there. On the machine that built it, 30 days of `conbus usage` counted 111
account stops over 34 hours, 513 tool failures and 245 permission asks — and
211 limit stops that were only three episodes, one a hop probe and one a single
unattended batch. So limits are not the headline. That machine is also
effectively single-vendor (91% of conversations are Claude Code), which is the
first thing the plan has to test: whether the buyer it assumes exists.

**What Choice 3 is not.** Not a hosted bus: the log stays on the machine and
the paid layer holds counts. Not a store of prompts, replies, tool bodies,
paths, commands or error text; not vendor tokens; not session files; not a
monitor that captures content. Not pooled quota or credentials. Not a way to
watch a person: a member's own view comes first, names are off unless the
organisation turns them on, and published cells hold at least five members.

**Validation first.** Nothing is hosted until the gates in
`docs/saas-strategy.md` pass: a reach count, a problem gate counted only in
dated incidents, a value gate counted only in commitments that cost the buyer
something, and money that has moved. Until then the product is the local tool,
and the README keeps its sentence — "conbus is a local tool. There is no hosted
bus, account or price." — because it describes what exists.

## Fall back

- To **Choice 1** if a gate fails, if the value gate has not passed by
  2026-12-22, or if the merged view ships for free elsewhere.
- **Reassess within two weeks** if a vendor with ≥30% of a pilot's turns adds
  limit-stop counts to its own team dashboard, or a funded competitor ships a
  counts-only mode.
- **Option 2** reopens only when `conbus verdict` scores a window with organic
  routing hops at or above the kill criterion, as before.

## Previous choice (kept)

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

*Correction, 2026-09-23 (later).* The ledger added the same day shows limit
stops on hooked, attended legs: 3 in three terminal-CLI conversations on
2026-09-05, and 8 on 2026-09-08 — 7 in five desktop-app conversations and 1 in
the CLI — in the hour of a hop probe. The sentence above undercounted them;
whether the three on 2026-09-05 were organic is not recorded. The conclusion
about Option 2 does not change: none of them produced an organic hop.

## Done when

"Choice" names one option; the data contract (`docs/rfd/0002-metadata-sync.md`)
exists before any sync code; and no host, account, or billing code was added in
the turn that wrote the choice.

## Do not

- Start the host in the turn that writes or changes the choice.
- Send a field the data contract does not name.
- Pool quota or credentials across people.
- Parse vendor session files on the live path.
- Pull another project's code into this repo.
- Treat another live-failure writeup as this decision.
