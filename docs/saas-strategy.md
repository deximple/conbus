# SaaS strategy — an ops ledger for coding agents

Written 2026-09-23 and revised the same day after an adversarial review. The
decision it serves is Choice 3 in [`saas-hypothesis.md`](saas-hypothesis.md);
the data it may ever send is [RFD 0002](rfd/0002-metadata-sync.md). Nothing
here starts a host. Market facts are as of 2026-09-23 and move quarterly.

## In one paragraph

Recording agent sessions across vendors is common, and so are memory, briefing
and handoff. Vendors ship their own admin analytics, below the top plans too,
and at least one product already sells a cross-vendor usage view by capturing
content through hooks it deploys with device management. What is left is
narrower than it first looked: **a view of where agents stop, fail and wait on
a person, merged across vendors, built from counts alone** — nothing that could
carry a prompt, a path or a command. conbus records exactly that locally. The
plan tests, cheaply and in order, whether anyone needs it, in which form (a
hosted page, or a feed into the monitoring a team already runs), and whether
they will pay. Nothing is hosted until money has moved; if it does not, conbus
stays a local tool.

> conbus is the flight recorder for coding agents. See where your agents stop,
> fail, or wait on a person — across vendors — without a prompt leaving the
> machine.

## What is already covered

| Layer | Who covers it | What is left |
| --- | --- | --- |
| Capturing sessions across vendors | Hooks are standard in Claude Code, Codex, Cursor, Copilot CLI and Gemini CLI. Free tools record whole sessions through them: claude-mem (~94k stars), agentmemory (~29k), SpecStory, Hivemind; Entire does it into git with a $60M seed | Nothing to sell. Capture is the entry fee |
| Memory, briefing, handoff | Vendor-native: Claude Code Projects and auto memory, Cursor Projects, Copilot Memory, Codex memories. Cross-vendor: the tools above, and AGENTS.md (20+ tools, Linux Foundation). GitHub's VS Code hands sessions between agents | Governed facts (source, approval, expiry, enforced before a tool runs) — demand unproven |
| Each vendor's own stops and admin views | Claude Code documents limit and account stops in its hooks (`StopFailure`: `rate_limit`, `billing_error`, `account_on_hold`, …) and exports API errors and cost through OpenTelemetry; Claude Team and Enterprise have usage analytics per person, and the Claude Code Analytics API serves any Console organisation (not individual accounts); Copilot has an organisation usage dashboard; Cursor's Admin API gives spend per person per day | Each covers one vendor. None merges stops, tool failures and permission friction across vendors |
| Cross-vendor usage by person | MintMCP Agent Monitor: hooks deployed by device management across Claude Code, Cursor, Codex and Copilot; usage and spend by person; captures prompts, commands, file and MCP calls | A counts-only alternative, for teams that will not send content |
| Suites | Datadog Agent Console, CloudWatch Coding Agent Insights, Grafana — up to three vendors each, from vendor telemetry | The signals their feeds lack; a place for conbus's counts to land |
| Engineering intelligence | DX (bought by Atlassian for $1B), Jellyfish, Swarmia, LinearB, Faros, Span — adoption, spend and AI share of PRs from admin APIs and git | Daily aggregates; no stops, failures or friction |
| Security | MintMCP, Lasso, Nightfall; Coder Agent Relay for self-hosted agents | Content capture and blocking — a different product and buyer |

## The evidence, weighed

- **Several tools, one agent.** 70% of developers use two to four AI tools at
  once (Pragmatic Engineer, n=906) — but that counts chat and autocomplete. For
  agents specifically, 69% run a single one and 63% rarely or never let one run
  unsupervised (Stack Overflow, n≈1,100, April 2026). At work, Claude Code is
  used by 39%, Copilot 21%, Codex 16%, Cursor 12%, OpenCode 7% (JetBrains,
  n>15k). Copilot is not a harness conbus supports.
- **The machine that built conbus is effectively single-vendor**: over 30
  days, 91% of its conversations and 99% of its tool failures were Claude Code,
  which already exposes these signals itself.
- **Quota is not the headline.** Over 30 days the ledger counted 211 Claude Code
  limit stops — **three episodes**, one of them a hop probe and one a single
  unattended batch. What the log has in volume is elsewhere: 111 account stops
  (authentication failures) spread over 34 hours, 513 tool failures, 245
  permission asks, and 2,260 of 2,534 Claude Code conversations unattended. The
  exhaustions that did happen organically were on surfaces with no hook
  (Codex.app, agy); their turns arrive by gap fill, which the ledger does not
  count yet.
- **Spend is large and fragmented**: $200–600 per developer per month all-in
  (DX), Copilot on usage-based billing since 2026-06-01.

So the lead is the unattended run that stopped on an account, the tool that
keeps failing, and the agent waiting on a person — across vendors — with limit
stops second, counted in episodes.

## Where conbus fits

| conbus has | The gap it meets |
| --- | --- |
| Stops filed by what an operator would do — limit (in episodes), account, vendor, request, other, a person — across five harnesses | Each vendor files its own, in its own words |
| Permission asks and refusals, tool failures and interrupts, per harness | Nobody merges them across vendors; Cursor exports none |
| Attended vs unattended | Unattended runs are the ones that fail silently — but only Claude Code says, and an agent started inside a Claude Code session inherits its answer |
| Codex account rate limits (app-server) and Claude's remaining quota (status line) | Remaining-quota readings reach no vendor's telemetry; the stops themselves do |
| One append-only local log, retention levels, sealed tool bodies | Forensics without sending content anywhere |
| `conbus usage` rows and the `--shareable` preview, both pinned by tests | The payload a counts-only sync would send, readable first |

What it does not have yet: cost and tokens; a Linux build; Copilot; any team feature; gap-filled activity in
the ledger; exclusion of its own probe traffic; one end per Cursor turn at the
adapter (a single Cursor exhaustion can be written three times, and one hook
matches quota words in the assistant's own reply).

Being honest about the moat: capture is not one. What can be defended is the
**normalisation** (five vendors' quirks pinned by live-traffic fixtures — every
Grok event reached the log twice on the machine that built it, Claude Code's
`prompt_id` is a process-global slot, Cursor reports one end from two hooks),
the **privacy architecture** (counts only, a payload you can print first,
one-way), and the **local log as the source of truth**. The free tool contains
the hard part, so a lead could pipe the rows into Grafana in an afternoon: the
paid part has to be something the rows alone do not give.

## Who pays first, and how to find out fast

- **Hypothesis:** an engineering or platform lead at an organisation of 10–200
  developers that pays for two or more of the five vendors conbus supports, each
  with at least 20% of its agent use, on plans without a merged view.
- **V0, the fastest falsifier (days):** of the first 30 organisations of that
  size the operator can reach, count how many meet that. Fewer than 6 of 30 and
  the wedge is too narrow: change the profile before interviewing anyone.
- **In pilots:** if the second vendor is under 10% of turns in two of three
  teams, "across vendors" is not the value, and the product is a single-vendor
  reliability report — which the vendors will ship themselves.
- **Dogfood and design partner:** operators who run agents unattended. Scoped
  to **unattended runs on Macs** until there is a Linux build; cloud and CI
  agents are out of reach today.
- **Not now:** individuals as payers (they use free tools; one orchestrator
  whose users were mostly free shut down in April), security buyers (they want
  content capture and blocking), enterprises (sales-led; the audit tier needs a
  tamper-evident log first).

## Two ways to deliver it — V1 decides

| | **Screen** | **Pipe** |
| --- | --- | --- |
| What | A hosted team page built from shared rows | The recorder emits its normalised counts as OpenTelemetry into what the team already runs — Datadog, Grafana, CloudWatch |
| Revenue | Subscription | A fleet edition (device-management package, managed settings, Linux, support) and partnerships with suites |
| Cost data | Needs vendor telemetry joined later | Arrives in the same collector from the vendors' own telemetry; conbus holds no keys |
| Burden on one operator | Hosting, a DPA, residency, security review | None of those |
| Weakness | Competes with bundled dashboards and DIY | Indirect revenue |

In V1, ask "where should this show up?". If at least half of the qualified
leads name a tool they already run, and at least one would pay for a supported
deployment — or a suite agrees to take the feed — the pipe goes first. Either
way the recorder and the counts are the same, and the OpenTelemetry export is
worth a local spike regardless: it needs no host.

## The product

| Tier | What | Price |
| --- | --- | --- |
| **Free** | The recorder and every local report — `usage`, `doctor`, `verdict`, briefings, hops. Open source once the repository is public | $0, always |
| **Team** (screen) | Member first: each member sees their own week — limit episodes on a declared plan, account stops in unattended runs, failing tools — and can forward it. Team totals by harness and day, with "N of M members reporting". A daily digest of stop and failure spikes. Optional automation labels | Tested below |
| **Fleet** (pipe) | Device-management and managed-settings package, Linux build, OpenTelemetry export, support | Tested below |
| **Enterprise** | Self-hosted control plane, SSO, audit export from each machine's log (after hash-chained segments and signed exports exist), retention controls | By quote, anchored above the top of the Team ladder |

Out of the first version on purpose: dollars (needs vendor telemetry),
same-day alerts (a daily send is what keeps activity times off the server —
RFD 0002), memory, briefing and hops (free and local), and any content.

The return has to be earned member first: *your week — two limit episodes on
Pro, nine account stops in the nightly run, Bash failing forty times in one
repository* — forwarded to whoever owns the plan or the automation. Without
cost data, right-sizing can only point at upgrades; it says so.

## Data boundary

Summarised here; the contract is [RFD 0002](rfd/0002-metadata-sync.md).

- The unit is the `conbus usage` row: a day, a harness, whether a person was
  there, and counts. Workspaces are off by default. Never a path, a prompt, a
  reply, a tool body, a command, an error text, or a vendor id.
- `conbus usage --shareable` prints exactly the rows a sync would send.
- Each UTC day is sent once, after it closes; no partial days, no IP addresses
  kept. Published cells hold at least five members.
- One-way: settings travel in the invite, and the service can only ask a
  machine to send less.
- Rows are pseudonymous personal data. A DPA and a DPIA template exist before
  the first pilot.

## Pricing and willingness to pay

Price is not what will stop a sale — teams spend $200–600 per developer per
month on these tools. Getting a daemon approved onto every machine is. So the
tests are about the unit and the bundle, and only money that moves counts.

- **Two shapes**, tested side by side: per billable member (a member with at
  least one synced turn that month) at $5, $8, $12, $15, $25 and $40 — the top
  two reach the operator's first proposal (Pro $15–25, Team $30–50) — and flat
  per organisation at $99, $249 and $499 a month.
- **Anchors**: Swarmia's "AI adoption and cost" module $5 per developer and its
  full platform $45–55; LinearB $29–59; Langfuse $29–2,499 a month; Copilot Pro
  $10; Paseo's hosted hub €15 per seat; Conductor Teams $60 per user. The two
  surveys behind this file disagree ($5–10 vs $15–30), which is why this is a
  test.
- **Behind the paywall**, things the free rows do not give: alert routing,
  history, coverage across a team, and — once there are enough organisations —
  benchmarks such as the median limit episodes per plan.
- Van Westendorp's four questions stay in the interviews as colour, not as a
  decision: twelve answers are noise.

## Gates and kill criteria

**Revised 2026-10-14: the ladder below replaces V0–V1.** One operator cannot
run twelve live interviews, so the problem gate is earned asynchronously, from
signals a stranger produces without a call. The money gate is V2 as before,
and the host gate is unchanged. The original table is kept underneath for the
record.

| Gate | Evidence | Pass | Kill |
| --- | --- | --- | --- |
| **E1 — exposure** (by 2026-11-04) | The repository public; a release with Linux and macOS binaries; the landing page live; at least two posts in places agent operators read | All four exist | — (a to-do, not a test) |
| **E2 — interest** (30 days after E1) | Waitlist signups, each with self-reported team size and the agent vendors the team pays for; release downloads from GitHub's own counts | ≥30 signups, of which ≥10 fit the buyer profile (10–200 developers, two or more supported vendors) | <10 signups after two separate posts: change the message or the profile before anything else |
| **E3 — problem** (by 2026-12-22) | From the waitlist: a pasted `conbus usage --shareable` week, or the five-question form with a dated incident in the last 30 days, its cost and the workaround | ≥5 fitting signups show an account stop in an unattended run, a failing tool or a permission wait they say cost them something; ≥3 ask for a pilot | ≤2 |
| **V2 — money** (by 2027-01-31) | A prepaid pilot of at least $250, or a signed order form | ≥1 | 0: fall back to Choice 1 |

- Fake-door price clicks on the landing page (per member at $8 and $15, flat
  at $249 per organisation, one shown at random per visitor) are colour, not a
  gate: a click costs nothing.
- At most a third of E2 and E3 from the operator's own network, as before.

The original gates, superseded:

| Gate | Pass | Kill |
| --- | --- | --- |
| **V0 — reach** (desk count, by 2026-10-07) | ≥6 of the first 30 reachable organisations meet the buyer profile | <6 of 30: change the profile first |
| **V1 — problem** (12 interviews, by 2026-11-04) | ≥5 of 12 describe a **dated incident in the last 30 days** with a stated cost and a named workaround, **and** ≥3 commit to a pilot — which counts only when ≥3 devices send rows within 7 days and a week-2 readout with the budget holder is on the calendar | ≤2 of 12 |
| **V2 — value** (≥3 pilots, by 2026-12-22) | ≥2 of 3 send week-2 rows without a reminder **and** forward the report or change a plan or an automation; **and** ≥1 moves money at the tested price — a prepaid pilot of at least $250, or an order form starting by 2027-01-31 | 0 of 3 |
| **Host** | V1 and V2 passed, ≥$1,000 a month in signed commitments, and the operator has chosen hosting, billing and the legal texts | — |

- **The middle has a rule.** V1 at 3–4 of 12: six more interviews with a changed
  profile, then decide.
- **Against politeness.** Screen on profile only, and log how many leads were
  contacted per qualifier. At most a third of interviews and one pilot from the
  operator's own network. At least one pilot runs on the defaults alone; if its
  report changes no decision, change the defaults or the product.
- **Deployment is logged separately.** "Blocked by security review" is a finding
  about packaging, not about demand.
- **Time box:** if V2 has not passed by 2026-12-22, fall back to Choice 1.
- **Reassess within two weeks** if a vendor with ≥30% of a pilot's turns adds
  limit-stop counts to its own team dashboard, if a funded competitor ships a
  counts-only mode, or if a platform ships the merged view for free — the
  likeliest being GitHub, whose Agent HQ already runs Claude and Codex.

## Before the first pilot

1. A release with `conbus usage` and `--shareable` (merged to main; not yet in
   a release).
2. A notarized binary, and a managed-settings or device-management package:
   Claude Code's `allowManagedHooksOnly` blocks user-installed hooks in managed
   fleets, which are the organisations that pay. Codex needs each developer to
   trust the hook once.
3. A one-page security packet: what runs, what it writes, what it would send,
   how to remove it.
4. The repository public, or the source available to pilots.
5. A DPA and a DPIA template.

## Capacity

One operator carries the history rewrite, the interviews, the pilots, the legal
texts and the builds, beside other live systems. The operator sets a weekly hour
budget, and the slip rule protects the gates: **fewer than 6 interviews by
2026-10-21 cuts the Linux build, not the interviews.**

## 90 days

| When | Work | Who |
| --- | --- | --- |
| to 2026-10-07 | `conbus usage` (#29, merged); `--shareable` and stops filed for an operator (#31); RFD 0002 revised; the validation kit revised; the Grok double delivery fixed at its source | agent |
| | V0 desk count; history scrub and public visibility; who to contact; the weekly hour budget | operator |
| to 2026-11-04 | 12 interviews → V1, including "where should this show up?" | operator runs, agent synthesises |
| | Release 0.2.0; notarization path; managed-settings package; security packet; gap-filled turns counted as `recovered`; probe traffic excluded; one end per Cursor turn and quota only from structured fields; Linux build (the first thing cut by the slip rule); `conbus verdict` closes 2026-09-28 and is recorded | agent |
| to 2026-12-02 | If V1 passed: pilots. Regardless: a local OpenTelemetry export spike (the pipe); RFD 0003 — authentication, rotation, revocation, replay protection, TLS, residency, encryption at rest, backups, deletion with a receipt — before any sync code | both |
| to 2026-12-22 | V2 → screen, pipe, or neither. On go, the operator decides hosting, billing, terms and privacy policy, and the host is started in a later turn, per `HANDOFF.md` | operator decides |

## Risks

| Risk | Answer |
| --- | --- |
| A platform absorbs the view (GitHub first, then Anthropic) | Stay neutral and local; feed the suites rather than compete with them; the gates are short |
| Teams standardise on one agent | V0 and the pilot falsifier find out in days, not months |
| Vendors move teams to metered billing, where a "block" is a budget the admin set and already sees | Lead with account stops, failures and friction, not limits |
| MintMCP or a vendor adds a counts-only mode | A reassess trigger; the local log and the fixtures are the head start |
| DIY from the free rows | Charge for what rows alone do not give: routing, history, coverage, benchmarks — or for the fleet edition |
| Installing a daemon on every machine | Notarized binary, managed-settings package, security packet, reversible `uninstall`, open source |
| Developers read it as surveillance | Counts only; own view first; cells of at least five; the payload is printable |
| Wrong numbers from vendor quirks | Normalisation is the product: fixtures from live traffic, twins counted once, stops filed by cause, known gaps named in the report |
| One operator's time | Nothing hosted before V2; the slip rule; every earlier step is local and reversible |

## Decisions only the operator makes

1. The V0 count, who to contact, and sending any message to them.
2. Scrub the history and make the repository public (`docs/publication-review.md`).
3. The weekly hour budget.
4. Whether to publish a landing or pricing page, and where.
5. The security packet, the DPA and the DPIA template.
6. Hosting, domain, billing provider, terms of service, privacy policy.
7. Any price shown to a customer.

## Sources

- Tools and agents in use: [Pragmatic Engineer, AI tooling 2026](https://newsletter.pragmaticengineer.com/p/ai-tooling-2026); [JetBrains agent adoption 2026](https://blog.jetbrains.com/research/2026/08/ai-coding-agent-adoption-2026/); [Stack Overflow, agents on a leash](https://stackoverflow.blog/2026/05/27/agents-on-a-leash-agentic-ai-remains-mostly-monitored-at-work/)
- Spend and billing: [DX, AI coding assistant pricing](https://getdx.com/blog/ai-coding-assistant-pricing/); [Claude Code costs](https://code.claude.com/docs/en/costs); [Copilot usage-based billing](https://github.blog/news-insights/company-news/github-copilot-is-moving-to-usage-based-billing/)
- Vendor stops and admin views: [Claude Code hooks](https://code.claude.com/docs/en/hooks); [Claude Code monitoring](https://code.claude.com/docs/en/monitoring-usage); [Claude Code Analytics API](https://platform.claude.com/docs/en/manage-claude/claude-code-analytics-api); [Team and Enterprise usage analytics](https://support.claude.com/en/articles/12883420-view-usage-analytics-for-team-and-enterprise-plans); [Claude Code status line](https://code.claude.com/docs/en/statusline); [Copilot organisation usage dashboard](https://github.blog/changelog/2026-02-20-organization-level-copilot-usage-metrics-dashboard-available-in-public-preview/); [Copilot OpenTelemetry](https://docs.github.com/en/copilot/concepts/agents/opentelemetry); [Cursor Admin API](https://cursor.com/docs/account/teams/admin-api); [Gemini CLI telemetry](https://geminicli.com/docs/cli/telemetry/)
- Cross-vendor monitoring: [MintMCP Agent Monitor](https://www.mintmcp.com/agent-monitor) and [its MDM deployment](https://www.mintmcp.com/docs/claude-code-hooks-kandji)
- Suites and tracing: [Datadog AI Agents Console](https://docs.datadoghq.com/ai_agents_console/); [CloudWatch coding agent insights](https://docs.aws.amazon.com/AmazonCloudWatch/latest/monitoring/coding-agents-insights.html); [Langfuse pricing](https://langfuse.com/pricing)
- Engineering intelligence: [Swarmia pricing](https://www.swarmia.com/pricing/); [LinearB pricing](https://linearb.io/pricing); [Atlassian buys DX](https://getdx.com/blog/dx-is-joining-atlassian/)
- Capture and memory: [claude-mem](https://github.com/thedotmack/claude-mem); [agentmemory](https://github.com/rohitg00/agentmemory); [Entire CLI](https://github.com/entireio/cli) and [its seed round](https://www.geekwire.com/2026/former-github-ceo-launches-new-developer-platform-with-huge-60m-seed-round/); [AGENTS.md](https://agents.md)
- Platforms: [Agent HQ](https://github.blog/news-insights/company-news/pick-your-agent-use-claude-and-codex-on-agent-hq/); [VS Code agent sessions](https://code.visualstudio.com/docs/agents/concepts/sessions); [Claude Projects redesigned](https://claude.com/blog/projects-redesigned); [Cursor Projects](https://cursor.com/changelog/projects); [Anthropic on subscription credentials in third-party tools](https://alternativeto.net/news/2026/2/anthropic-officially-bans-using-subscription-authentication-for-third-party-claude-use)
- Orchestrators: [Conductor pricing](https://www.conductor.build/pricing); [Paseo Hub](https://paseo.sh/hub); [Vibe Kanban after Bloop](https://nimbalyst.com/blog/vibe-kanban-after-bloop-whats-next/)
