# SaaS hypothesis

Date: 2026-09-22. Status: **Choice 3, written 2026-09-23**, replacing Choice 1
of the same day. No host exists, and this file starts none.

Read `AGENTS.md` and `HANDOFF.md` first. This file does not reopen the constraints there. Vendor TUIs stay the window. Tokens stay out of conbus. Prompts are not uploaded.

## Gap

conbus keeps one conversation id while the harness changes, and it is local on purpose. A SaaS that stored the conversation would break that design. No sentence yet says who pays, for which hop, after the design is left as it is.

## Options

1. **Local tool.** There is no SaaS hypothesis. Say that in the README in one sentence. Stop.
2. **SaaS on the hop.** The server may store only `bus_id`, the hop, and the quota fact. It does not store prompt bodies, vendor tokens, or session files. The paid event is one quota hop after which the same `bus_id` still identifies the conversation. Price attaches to that event only.
3. **Open-core ops ledger.** The recorder stays local and free. A paid, hosted
   control plane receives counts and nothing else — the rows `conbus usage
   --json` prints, transformed as `docs/rfd/0002-metadata-sync.md` says — and
   shows a team where its agents are blocked, failing, or waiting on a person,
   across vendors. The paid event is a team seeing that. Price attaches to
   active developers, not to hops.

## Choice

**3 — Open-core ops ledger, validation first.** Written 2026-09-23.

**Why it moved from 1.** Choice 1 answered whether the hop could be sold, and
that answer stands: Option 2 prices an event that has not happened organically
once (zero organic hops in the dogfood window), so it stays closed. What
changed is the question. The operator asked on 2026-09-23 what else the
recorder is, and three landscape surveys (summarised in
`docs/saas-strategy.md`) found that most of what it does is already done
elsewhere, free, with one exception the local log already covers:

- Capture is a commodity. Hooks are standard across vendors, and several free
  tools with tens of thousands of stars already record whole sessions across
  them. Briefing, memory and handoff are crowded and being absorbed by the
  platforms. None of these is the paid event.
- No vendor documents a quota-stop event; Claude Code's rate limits reach only
  the status line; admin views are gated to top-tier plans; and most
  developers run two to four tools at once. So a team on mixed vendors below
  the top tier has no single view of which agents are blocked, failing, or
  waiting for a person. The recorder already classifies stops and permission
  asks across five harnesses, and knows whether a person was there.
- On the machine that built it, `conbus usage` over 15 days counted about
  2,400 conversations across six harnesses, 200 quota-stopped turns (one
  unattended burst on 2026-09-16), about 240 permission asks and about 200
  tool failures — none of which any vendor dashboard showed in one place. It
  also found what it cannot show: no hook carries tokens or prices.

**What Choice 3 is not.** Not a hosted bus: the log stays on the machine and
the server holds counts. Not a store of prompts, replies, tool bodies, paths,
commands or error text; not vendor tokens; not session files. Not pooled quota
or credentials. Not a way to watch a person: per-person views belong to that
person unless the organisation turns names on, and team views hide groups of
fewer than three.

**Validation first.** The host waits for the gates in `docs/saas-strategy.md`:
a problem gate (interviews), then a value gate (hand-run pilots that send the
same rows). Until both pass, the product is the local tool, and the README keeps
its sentence — "conbus is a local tool. There is no hosted bus, account or
price." — because it describes what exists.

## Fall back

- To **Choice 1** if the problem gate or the value gate fails, if neither has
  passed by 2026-12-22, or if a platform ships the team view of limits and
  failures across vendors for free.
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
