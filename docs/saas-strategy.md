# SaaS strategy — an ops ledger for coding agents

Written 2026-09-23. The decision it serves is Choice 3 in
[`saas-hypothesis.md`](saas-hypothesis.md); the data it may ever send is
[RFD 0002](rfd/0002-metadata-sync.md). Nothing here starts a host. The market
facts are as of 2026-09-23 and move quarterly; "unverified" marks a figure with
one secondary source.

## In one paragraph

Recording coding-agent sessions across vendors is no longer rare, and neither
is memory, briefing or handoff. What no vendor offers, and what teams on mixed
vendors below the top plans cannot get anywhere, is one view of **which agents
are blocked, failing, or waiting on a person** — quota stops, policy stops,
tool failures, permission asks, unattended runs — across Claude Code, Codex,
Cursor, Grok and OpenCode. conbus already records exactly that, locally. The
strategy is to keep the recorder free and open, prove that teams want the view,
and only then sell a hosted control plane that receives **counts, never
content**. If teams do not want it, conbus stays a local tool.

> conbus is the flight recorder for coding agents. See which agents are blocked,
> failing, or waiting on a person — across vendors — without a prompt leaving
> the machine.

## What is already covered

| Layer | Who covers it | What is left |
| --- | --- | --- |
| Capturing sessions across vendors | Hooks are standard in Claude Code, Codex, Cursor, Copilot CLI and Gemini CLI. Free tools record whole sessions through them: claude-mem (~94k stars), agentmemory (~29k), SpecStory, Hivemind; Entire does it into git with a $60M seed | Nothing to sell. Capture is the entry fee |
| Memory, briefing, handoff | Vendor-native: Claude Code Projects and auto memory, Cursor Projects, Copilot Memory, Codex memories. Cross-vendor: the tools above, plus AGENTS.md (20+ tools, Linux Foundation) for instructions. GitHub's VS Code hands sessions between agents | Governed facts (source, approval, expiry, enforced before a tool runs) — demand unproven |
| Telemetry per vendor | Claude Code OpenTelemetry (cost, tokens, tool decisions and errors; lockable by admins), Codex OpenTelemetry (defaults only), Copilot OpenTelemetry and session streaming (Enterprise), Cursor Admin API (tokens and cents; no tool calls, no OTel), Gemini CLI OpenTelemetry | One schema across vendors; no vendor documents a quota-stop event; Claude's rate limits reach only the status line |
| Suites | Datadog Agent Console, CloudWatch Coding Agent Insights, Grafana — up to three vendors each, mostly metrics | The signals their vendor feeds lack |
| Engineering intelligence | DX (bought by Atlassian for $1B), Jellyfish, Swarmia, LinearB, Faros, Span — adoption, spend and AI share of PRs from admin APIs and git | Daily aggregates; no failures or stops; inherit every admin-API gate |
| Security | MintMCP Agent Monitor, Lasso, Nightfall; Coder Agent Relay for self-hosted agents | Content capture and blocking — a different product and buyer |

Admin views are gated to the top plans: Anthropic's Admin API is unavailable to
individual accounts, Copilot usage records need Enterprise Managed Users,
Cursor's analytics are Enterprise-only. Meanwhile 70% of developers use two to
four AI tools at once and 15% five or more; about 30% hit usage limits and switch
tools when they do; all-in spend is $200–600 per developer per month; and Copilot
moved to usage-based billing on 2026-06-01. Sources at the end.

## Where conbus fits

What the local log already holds that the gaps above ask for:

| conbus has | The gap it meets |
| --- | --- |
| Turn stops classified across five harnesses: quota, policy/billing/auth, a person | No vendor documents a quota-stop event |
| Codex account rate limits (app-server) and Claude's `rate_limits` (status line) | Only single-user tools chart quota burn |
| Permission asks and refusals, tool failures and interrupts, per harness | Each vendor reports its own, in its own schema; Cursor exports none |
| Attended vs unattended, from the entrypoint | Automation runs where nobody is watching are the ones that fail silently |
| One append-only local log, retention levels, sealed tool bodies | Audit and forensics without sending content anywhere |
| `conbus usage` rows: counts only, pinned by a test | The shape a metadata-only sync would send |

What it does not have: cost and tokens (no hook carries them — they come from
vendor telemetry, later), a Linux build (teams and CI run Linux), and any team
feature at all.

Being honest about the moat: capture is not one. What can be defended is the
**normalisation** (five vendors' quirks pinned by live-traffic fixtures — on the
machine that built it every Grok event reached the log twice, Claude Code's
`prompt_id` is a process-global slot, Cursor reports one end from two hooks),
the **privacy architecture** (counts only, a payload you can print before it is
sent, one-way), and the **local log as the source of truth**, which is what an
audit product later needs.

## Who pays first

- **Hypothesis to test first:** an engineering or platform lead at an
  organisation of 10–200 developers that runs two or more agent vendors on plans
  below the enterprise tier. They pay $200–600 per developer per month across
  vendors and no dashboard covers the mix.
- **Dogfood and design partner:** operators who run agents unattended (SDK
  scripts, CI, overnight runs). On the machine that built conbus, 2,147 of 2,265
  Claude Code conversations in 15 days were unattended, and the 200 quota stops
  came from one of those bursts.
- **Not now:** individuals as payers (they use free tools, and one orchestrator
  whose users were mostly free shut down in April), security buyers (they want
  content capture and blocking), and enterprises (sales-led, later).

## The product

| Tier | What | Price hypothesis |
| --- | --- | --- |
| **Free** | The recorder and every local report — `usage`, `doctor`, `verdict`, briefings, hops. Open source once the repository is public | $0, always |
| **Team** | Hosted view of the synced rows: quota and policy stops by agent and member, blocked-time estimates where the reset is known, permission friction and tool failures, unattended runs, alerts on stop and failure spikes, 90 days of history | $8 per active developer per month, testing $5–15 |
| **Enterprise** | Self-hosted control plane, SSO, audit export from each machine's local log on request, retention controls, governed constraints | By quote, anchored at $20+ per developer |

Out of the Team MVP on purpose: cost in dollars (phase 2, from vendor
OpenTelemetry received locally and joined to sessions), memory and briefing
(free and local), hops (free and local), any content.

The ROI sentence the Team tier has to earn: *you pay for N seats of each vendor;
here is who is blocked on which plan and how often, and which automations fail
overnight* — plan right-sizing and saved hours, measured, not claimed.

## Data boundary

Summarised here; the contract is [RFD 0002](rfd/0002-metadata-sync.md).

- The unit is the `conbus usage` row: a day, a harness, whether a person was
  there, and counts. Workspace identity is off by default, hashed per
  organisation when on, and a repository name only when the organisation says
  so. Never a path, a prompt, a reply, a tool body, a command, an error text,
  or a vendor id.
- The payload is the preview: what `conbus usage --json --shareable` prints is
  byte for byte what would be sent.
- One-way: the service acknowledges and can send nothing back — no commands,
  no configuration, no code.
- Opt-in per machine, revocable, and deletable server-side.

## Pricing and willingness to pay

Anchors: Swarmia's "AI adoption and cost" module $5 per developer, its full
platform $45–55; LinearB $29–59; Langfuse $29–2,499 per month; Copilot Pro $10;
Paseo's hosted hub €15 per seat; Conductor Teams $60 per user. The two surveys
behind this file disagree on the band ($5–10 vs $15–30 per developer), and the
operator's first proposal was higher still (Pro $15–25, Team $30–50 per user).
The hypothesis starts at $8 because the Team tier sells counts, not an agent,
and the nearest thing on sale — a cost-and-adoption module — is $5; there is no
individual Pro tier because individuals use free tools. Those are reasons to
test, not to pick, so the ladder below reaches the operator's band.

The experiment, in the interviews and the pilots below:

1. Van Westendorp's four questions (too cheap, cheap, expensive, too expensive)
   per active developer per month.
2. A Gabor–Granger check at $5, $8, $12, $15 and $25: "would your team pay
   this?"
3. At the end of a pilot, a paid ask: a letter of intent or a 30-day paid pilot
   at the price the answers point to.
4. Only if the operator publishes a page: a pricing fake door (Team vs
   Enterprise) with click-through per tier.

## Gates and kill criteria

| Gate | Pass | Kill |
| --- | --- | --- |
| **V1 — problem** (12 interviews with the first-buyer profile) | ≥5 of 12 describe a limits, failures or visibility pain at least monthly that they handle by hand or not at all, **and** ≥3 agree to a two-week pilot on ≥3 machines | ≤2 of 12 describe it |
| **V2 — value** (hand-run pilots with ≥3 teams: they run the free recorder and send the shareable rows weekly; the report is produced by hand) | ≥2 of 3 ask for the week-2 report without being prompted, **and** ≥1 agrees to pay at ≥$5 per developer | 0 of 3 ask again |
| **Host** | V1 and V2 passed, and the operator has chosen hosting, billing and the legal texts | — |

Time box: if V2 has not passed by **2026-12-22**, fall back to Choice 1.
Platform trigger: if a platform ships the cross-vendor team view of limits and
failures for free — the likeliest is GitHub, whose Agent HQ already runs Claude
and Codex — reassess within two weeks.

## 90 days

| When | Work | Who |
| --- | --- | --- |
| to 2026-10-07 | `conbus usage` (shipped); the Grok double-delivery fixed at its source; RFD 0002; `--shareable` preview; the validation kit (screener, outreach, interview guide, pilot protocol, landing copy) | agent |
| | Repository history scrub and public visibility (the recorder has to be inspectable for the privacy claim to mean anything); choose who to interview | operator |
| to 2026-11-04 | 12 interviews → V1; a Linux build of the recorder (systemd user service); quota signals hardened (Claude desktop has no status line); `conbus verdict` closes on 2026-09-28 and is recorded | operator runs interviews, agent synthesises and builds |
| to 2026-12-02 | If V1 passed: hand-run pilots with three teams; RFD 0003 (control-plane design, no host); a spike on receiving vendor OpenTelemetry locally for cost | both |
| to 2026-12-22 | V2 → go or no-go. On go, the operator decides hosting, billing, terms and privacy policy, and the host is started in a later turn, per `HANDOFF.md` | operator decides |

## Risks

| Risk | Answer |
| --- | --- |
| A platform absorbs the view (GitHub first, then Anthropic) | Stay neutral and local; export to the suites rather than compete with them; the gates are short so the loss is small |
| Price pressure from $5 add-ons and bundled dashboards | Rows are tiny, so cost to serve is small; the enterprise audit tier is where the price is |
| Installing a daemon on every developer's machine | One binary, `install --apply`, reversible `uninstall`, open source; Linux next |
| Developers read it as surveillance | Counts only; per-person views belong to the person; groups under three are hidden; the payload is printable |
| Vendor quirks make the numbers wrong | Normalisation is the product: fixtures pinned by live traffic, duplicate delivery counted once, stops never folded into completions |
| One operator's time | Nothing hosted before V2; every step before that is local and reversible |

## Decisions only the operator makes

1. Scrub the history and make the repository public (`docs/publication-review.md`).
2. Who to interview, and sending any message to them.
3. Whether to publish a landing page or pricing page, and where.
4. Hosting, domain, billing provider, terms of service, privacy policy, DPA.
5. Any price that is shown to a customer.

## Sources

- Multi-tool use and limits: [Pragmatic Engineer, AI tooling 2026](https://newsletter.pragmaticengineer.com/p/ai-tooling-2026); [Pragmatic Engineer, impact of AI 2026](https://newsletter.pragmaticengineer.com/p/the-impact-of-ai-on-software-engineers-2026); [JetBrains agent adoption 2026](https://blog.jetbrains.com/research/2026/08/ai-coding-agent-adoption-2026/); [Stack Overflow, agents on a leash](https://stackoverflow.blog/2026/05/27/agents-on-a-leash-agentic-ai-remains-mostly-monitored-at-work/)
- Spend: [DX, AI coding assistant pricing](https://getdx.com/blog/ai-coding-assistant-pricing/); [Claude Code costs](https://code.claude.com/docs/en/costs); [Copilot usage-based billing](https://github.blog/news-insights/company-news/github-copilot-is-moving-to-usage-based-billing/)
- Vendor telemetry: [Claude Code monitoring](https://code.claude.com/docs/en/monitoring-usage); [Claude Code Analytics API](https://platform.claude.com/docs/en/manage-claude/claude-code-analytics-api); [Claude Code status line](https://code.claude.com/docs/en/statusline); [Copilot OpenTelemetry](https://docs.github.com/en/copilot/concepts/agents/opentelemetry); [Cursor Admin API](https://cursor.com/docs/account/teams/admin-api); [Gemini CLI telemetry](https://geminicli.com/docs/cli/telemetry/)
- Suites and tracing: [Datadog AI Agents Console](https://docs.datadoghq.com/ai_agents_console/); [CloudWatch coding agent insights](https://docs.aws.amazon.com/AmazonCloudWatch/latest/monitoring/coding-agents-insights.html); [Langfuse pricing](https://langfuse.com/pricing)
- Engineering intelligence: [Swarmia pricing](https://www.swarmia.com/pricing/); [LinearB pricing](https://linearb.io/pricing); [Atlassian buys DX](https://getdx.com/blog/dx-is-joining-atlassian/)
- Capture and memory: [claude-mem](https://github.com/thedotmack/claude-mem); [agentmemory](https://github.com/rohitg00/agentmemory); [Entire CLI](https://github.com/entireio/cli) and [its seed round](https://www.geekwire.com/2026/former-github-ceo-launches-new-developer-platform-with-huge-60m-seed-round/); [AGENTS.md](https://agents.md)
- Platforms: [Agent HQ](https://github.blog/news-insights/company-news/pick-your-agent-use-claude-and-codex-on-agent-hq/); [VS Code agent sessions](https://code.visualstudio.com/docs/agents/concepts/sessions); [Claude Projects redesigned](https://claude.com/blog/projects-redesigned); [Cursor Projects](https://cursor.com/changelog/projects)
- Orchestrators: [Conductor pricing](https://www.conductor.build/pricing); [Paseo Hub](https://paseo.sh/hub); [Vibe Kanban after Bloop](https://nimbalyst.com/blog/vibe-kanban-after-bloop-whats-next/)
