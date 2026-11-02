# conbus manual

This manual describes **conbus 1.0.0-rc.3** on macOS and Linux. For the short version, read
the [README](../README.md); for what changed between releases, the
[CHANGELOG](../CHANGELOG.md). 한국어: [manual.ko.md](manual.ko.md)

- [1. What conbus is](#1-what-conbus-is)
- [2. Before you install](#2-before-you-install)
- [3. Install](#3-install)
- [4. Check that it works](#4-check-that-it-works)
- [5. Recording](#5-recording)
- [6. Briefings](#6-briefings)
- [7. Hops](#7-hops)
- [8. Quota](#8-quota)
- [9. Recovering history](#9-recovering-history)
- [10. Facts shared across checkouts](#10-facts-shared-across-checkouts)
- [11. Your data](#11-your-data)
- [12. Upgrading](#12-upgrading)
- [13. Uninstalling](#13-uninstalling)
- [14. Troubleshooting](#14-troubleshooting)
- [15. The local policy](#15-the-local-policy)
- [16. Claude Code profiles: a second account, switched by hand](#16-claude-code-profiles-a-second-account-switched-by-hand)
- [17. Reference](#17-reference)
- [18. Known limitations](#18-known-limitations)

---

## 1. What conbus is

You probably use more than one coding agent. Each one keeps its own history,
its own idea of what you were doing, and its own quota. When one of them stops
in the middle of a task — a five-hour limit, a weekly cap — the next one starts
from nothing.

conbus sits underneath the agents you already use. Every prompt, reply and
tool call goes into **one local log**, filed under a conversation id that stays
the same when the agent changes. From that log, conbus **briefs** a new session
on what recent sessions in the same checkout were doing, and it can **hop** a
conversation to another agent — on its own when a vendor stops a turn for
quota, or when you tell it to.

The agents' own terminals and apps stay the window. conbus has no chat UI.

### Words used in this manual

| Word | Meaning |
| --- | --- |
| **harness** or **leg** | One agent: `claude_code`, `codex`, `cursor`, `grok`, `opencode`, `agy` |
| **conversation** | One piece of work, possibly spanning several legs. Identified by a **bus id** (a UUID) |
| **recorder** | The `conbus daemon`, which receives every hook and appends to the log |
| **hook** | `conbus hook`, registered in each vendor's own hook settings. The vendor runs it on every event |
| **briefing** | Context conbus hands a new session through the vendor's own channel |
| **hop** | Moving a conversation to another leg. *Displaced* when a quota stop forced it; *handed off* when you asked |
| **warm** | A session on the next leg that conbus opens itself and keeps ready, so a hop lands in it |
| **takeover** | The note a hop leaves for `conbus run`, telling it which leg to start next in the same terminal |

---

## 2. Before you install

- **macOS** on Apple silicon or Intel, or **Linux** on x86_64 or aarch64. The recorder is
  a launchd agent on a Mac and a systemd user unit on Linux; it does not run
  on Windows yet.
- **At least one agent, already signed in** through its own binary: Claude Code
  (`claude`), Codex (`codex` or Codex.app), Cursor, the Grok Build CLI (`grok`),
  or opencode. conbus never signs in for you and never reads their credentials.
- **`~/.local/bin` on your `PATH`**, or be ready to add it. The hooks call the
  binary by its absolute path, so they work either way; your shell needs it to
  find `conbus`.

---

## 3. Install

### From a release

```bash
curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
```

The script:

1. downloads the release asset for this OS — `conbus-macos-universal.tar.gz`
   on a Mac, `conbus-linux-x86_64.tar.gz` or `conbus-linux-aarch64.tar.gz` on
   Linux, chosen by `uname -m` — from the latest release
   (falling back to `gh release download` when the anonymous download is
   refused);
2. checks it against its published SHA-256, and stops without installing
   anything if they differ;
3. places the binary at `~/.local/bin/conbus`, written beside and then renamed,
   so an agent in the middle of a hook is never cut off;
4. runs `conbus install`, which only **reports** what it would register.

Nothing in your agents' settings has changed at this point. Options:

| Variable | Effect |
| --- | --- |
| `CONBUS_VERSION=v0.2.8` | Install that release instead of the latest |
| `CONBUS_REPO=owner/name` | Install from a fork |
| `CONBUS_BASE_URL=…` | Download from a mirror, or `file:///path/to/dist` for a local build |

### From source

Rust 1.99 or newer:

```bash
git clone https://github.com/deximple/conbus
cd conbus
cargo build --release -p conbus
./target/release/conbus install
```

### Registering the hooks

```bash
conbus install --apply
```

For each agent **that is installed on this machine** — recognised by its own
configuration directory — conbus merges one hook into that agent's settings.
Agents that are not installed are skipped; run `install --apply` again after
installing one.

| Agent | File | What is registered |
| --- | --- | --- |
| Claude Code | `~/.claude/settings.json` | 13 events, and the StatusLine (wrapped, so your own status line still shows) |
| Codex | `$CODEX_HOME/hooks.json` (default `~/.codex`) | 7 events |
| Cursor | `~/.cursor/hooks.json` | `sessionStart`, `beforeSubmitPrompt`, `stop`, `sessionEnd`, `afterAgentResponse` |
| grok | `~/.grok/hooks/conbus.json` | 10 events |
| opencode | `~/.config/opencode/plugins/conbus.js` | a plugin that forwards session and message events |

It also places the binary at `~/.local/bin/conbus` if it is not there yet, and
writes the service that keeps the recorder running and starts it at login: a
launchd agent, `~/Library/LaunchAgents/sh.conbus.daemon.plist`, on a Mac, or a
systemd user unit, `~/.config/systemd/user/sh.conbus.daemon.service`, on
Linux. Where no `systemctl --user` answers — a container or a CI runner —
install still writes everything else and prints the one command that runs
`conbus daemon` by hand.

Every file it changes keeps a copy beside it (`…json.conbus-backup`), your own
hooks and settings stay exactly as they were — including their order — and a
file that is not valid JSON is reported and left untouched.

### Approving the hooks in Codex

Codex runs a new hook only after you approve it. Start Codex once; it lists the
hooks that are new or changed and asks whether to trust them. Until you do,
Codex hooks are registered and silently do nothing. conbus never writes that
approval for you — it is your consent, not conbus's.

### On a managed Mac

If your organisation manages Claude Code and its policy sets
`allowManagedHooksOnly`, the hooks `install --apply` registers do not run.
`conbus install --managed` prints the hooks block your administrator adds to
Claude Code's managed settings instead, and says where it goes; it writes
nothing. [Managed Macs](fleet.md) covers the rest: the binary, the recorder
each person still starts with `install --apply`, approving it as a login item,
and taking it all back out.

---

## 4. Check that it works

1. Open any agent in a project folder and send one prompt.
2. Run `conbus log --tail 5`. The turn is there.
3. Run `conbus doctor`.

`doctor` prints one line per fact. The ones that matter first:

| Line | Healthy |
| --- | --- |
| `build` | `0.2.8 (<commit>)` |
| `socket` | `listening` |
| `daemon` | the same commit as `build`. `DIFFERS` means the binary was replaced and the daemon was not restarted |
| `writable` | `appendable` |
| `hooks` / `events` | `runs` and `N/N registered` for each installed agent; `not installed here` for the others |
| `codex` | `7 hook(s) trusted`. `NOT TRUSTED` means you have not approved them in Codex yet |
| `last` | the turn you just sent |
| `drops` | not growing while you work |

The rest of `doctor`:

| Line | What it tells you |
| --- | --- |
| `hardening` | whether the recorder disabled core dumps and locked its memory |
| `backup` | when the last backup ran and where to, or that this log has one copy (section 11) |
| `chat` | how Claude.ai conversations can reach the log (section 9) |
| `log` | records, conversations, and lines the recorder could not read (should be 0) |
| `twins` | sessions recorded both live and by backfill, and which copy they continue under |
| `nameless` | a conversation an older build filed under an empty session id (section 14) |
| `briefing` | how many new sessions were briefed, and why the others were not (section 6) |
| `quota` | what each leg has left (section 8) |
| `hops` / `excluded` | hops that happened on their own, and hops that were typed or staged (section 7) |
| `inject` | how much context briefings and hops have added, as a cost |
| `turns` | turns written by people, by programs, and unlabelled |
| `route` | where a hop would go right now |
| `takeover` | a hop waiting for `conbus run`, and whether it is too old to follow |
| `warm` | sessions conbus opened on other legs and still holds |

`conbus hop --demo` records a pretend failover — no agent window opens — so
you can see what a hop looks like in the log before a real one happens.

---

## 5. Recording

Nothing to do: once the hooks are registered, the recorder runs in the
background and every event from every installed agent lands in the log.

**What is recorded** (by default): prompts and replies, which tools were called
and whether they succeeded, how long they took, permission requests, quota and
error stops, session starts and ends, and subagent turns. **Tool inputs and
outputs are not written** — only their size. See section 11 to change that.

**Reading it back:**

```bash
conbus log --tail 20                  # the last 20 records
conbus log --bus-id 01a0c7f2          # one conversation, every leg it touched (a prefix is enough)
conbus log --since 2026-09-20            # a date or any prefix of a timestamp
conbus log --grep "migration"         # records whose text contains this
conbus log --json                     # one JSON record per line, for scripts
conbus log --reveal --bus-id 01a0c7f2 # decrypt sealed tool bodies (section 11)
conbus log index                    # build the derived read cache under ~/.conbus/cache
```

Reads that never print a tool body run against a derived cache of digested
records under `~/.conbus/cache/`; it is rebuilt automatically on the first
read and `conbus log index` builds it explicitly. Deleting `cache/` loses
nothing. `CONBUS_NO_CACHE=1` reads the segments directly, as 0.2.x did.

When a hook cannot reach the daemon — a restart, a crashed recorder — it
writes its request to `~/.conbus/spool/` instead of losing the turn; the
next daemon drains it and deletes the file. `doctor` shows `spool` when
entries wait. Spooled records carry `hook_id` and `spooled_at`, so a
re-delivered event counts once.
```

**Counting it:** `conbus usage` folds the log into what your agents did, per
agent, over the last 7 days (`--days 30`, `--since 2026-09-01`, or one closed day
with `--day 2026-09-22`). Thirty days on the machine that built it:

```text
usage     from 2026-08-25 (UTC) · 131666 record(s), 4397 recovered · not counted: earlier 33138 · repeated 10610 · recorded twice 24
harness   claude_code — 2657 conversation(s): attended 240 · unattended 2354 · unknown 63
          3207 turn(s) · subagents 1016
          stopped: limit 211 in 3 episode(s) · account 111 · vendor 3 · request 0 · other 1 · by a person 0
          27982 tool call(s): failed 545 · interrupted 0 · refused 48 · asks 247 · 21h in tools
harness   codex — 183 conversation(s): attended 70 · unattended 0 · unknown 113
          1419 turn(s) · subagents 0
          stopped: limit 0 in 0 episode(s) · account 0 · vendor 0 · request 0 · other 0 · by a person 0
          1892 tool call(s): failed 0 · interrupted 0 · refused 0 · asks 1 · 0s in tools
failing   Bash 453 · Read 61 · Glob 4
busiest   /Users/you/project 2151 · /Users/you/other 145 (turns)
cost      not in this log: no hook payload carries tokens or prices
```

- **attended · unattended · unknown**: whether a person was at the session,
  from the front end it ran under. Only Claude Code says; another agent started
  from inside a Claude Code session inherits what it said, and the rest are
  `unknown`.
- **stopped**: why turns ended early, sorted by what you would do about it.
  **limit** — a usage limit (stops on one agent less than an hour apart are one
  **episode**); **account** — authentication, billing, a hold; **vendor** — the
  vendor was overloaded or failed; **request** — invalid, too long, no such
  model; **other** — a reason this version does not recognise; **by a person**.
- **subagents** are subagent runs that finished. They are not turns, and how a
  subagent ended is not counted as a stop.
- **failed · interrupted · refused · asks**: tool calls that ran and failed,
  that you stopped, that the agent refused to run, and the times you were
  asked for permission. **in tools** adds up the durations the agent reported;
  Codex reports none.
- **recovered**: records the recorder did not see as they happened but
  recovered later — turns missed while it was down (gap fill) and conversations
  `backfill` brought in, such as Codex.app's threads, which no hook reports.
  They count by when they happened, so history older than the window does not
  appear.
- **not counted**: records before or after the window; **repeated** — a second
  delivery of the same report, since with some agents one event reaches the log
  twice; **recorded twice** — a recovered copy of a session the hooks also
  recorded; **probe** — a run that declared itself a test of conbus.
- A Cursor turn that reported several stops — Cursor writes a turn's end from
  more than one hook — counts its most serious one. Cost is not counted at all:
  no hook carries tokens or prices.

`conbus usage --json` prints one row per day, agent, folder and attendance:
counts only, never a prompt, a reply or a tool body. `conbus usage --shareable`
prints the same rows with the folder removed — what a sync would send, if one
existed ([RFD 0002](rfd/0002-metadata-sync.md)). `--shareable` itself sends
nothing; the point is that you can read it first.

**Per folder:** `conbus progress` regroups the same counts by the folder the
work was in. For each one it names the agents that worked there, their turns
(and how many a person was there for), their tool calls and failures, the hops
and briefings that carried work from one agent to another, and the commits the
folder's `HEAD` gained in the window, asked of `git` with a five-second bound
per call and fifteen seconds for the whole report. A checkout and its
worktrees are one place, under the main checkout, and so is a worktree removed
since: it is placed by the folder it was in. A folder `git` cannot place stands
for itself and shows no commit count rather than a zero. The busiest eight are
printed, by turns a person was there for. `--workspace DIR` keeps one folder,
what is under it and, for a repository, its worktrees wherever they are; and
`--json` prints every one, one per line. Like `usage`, it holds counts and
never a body.

```bash
conbus progress --workspace .     # this checkout, the last 7 days
conbus progress --days 30 --json  # every folder, a month
```

**Into your own monitoring:** `--otlp URL` sends those same rows, as
OpenTelemetry metrics, to a collector on this Mac — the OpenTelemetry
Collector, Grafana Alloy and the Datadog Agent all listen on
`http://127.0.0.1:4318` — which forwards them wherever it is set up to send:
Grafana, Datadog, CloudWatch. Each count is a metric (`conbus.turns`,
`conbus.stops` by `kind`, `conbus.tool.failures` and the rest, listed in
[RFD 0002](rfd/0002-metadata-sync.md#the-pipe-the-same-rows-into-a-collector-the-person-runs)),
labelled with the agent and attendance. Nothing is sent without the flag.

```bash
conbus usage --day 2026-09-22 --shareable --otlp http://127.0.0.1:4318   # print a closed day's rows, and send them
```

`--day` is the unit to send: yesterday, once, after it has closed. Each point
is what happened that day, so a day sent twice is counted twice, and today is
still growing — conbus says so if you send it. Send soon after 00:00 UTC: some
backends drop points stamped hours ago (Datadog keeps none more than an hour
old unless Historical Metrics Ingestion is on). This first version speaks plain
HTTP to a collector on this Mac only — `https://` and other hosts are refused,
and TLS onward is the collector's job — and it does not retry: a refused URL, a
failed connection, an answer other than 2xx, or points the collector says it
rejected are errors, and the command exits non-zero.

A record looks like this (fields vary by event):

```json
{"v":3,"ts":"2026-09-22T19:23:42.504Z","seq":144032,"bus_id":"01a0c7f2-4b1e-7d20-9c3a-5e8f1a2b3c4d",
 "harness":"claude_code","vendor_session_id":"4e2f…","workspace":"/Users/you/project",
 "entrypoint":"cli","event":"tool_requested","tool":"Bash","tool_use_id":"toolu_…",
 "input":{"kind":"elided","bytes":464}}
```

**Programs are told apart from people.** Sessions started by an SDK, an MCP
client or a GitHub Action are marked unattended; prompts that a vendor itself
labels as machine-written are marked too. They are recorded like any other,
but they are never briefed and never hopped.

**If the recorder is down,** a hook still returns immediately with a
well-formed answer, so your agent is never blocked; the event is dropped and
noted in `~/.conbus/drops.jsonl`, and `doctor` counts it. When the recorder
starts again it fills in, from the agents' own transcripts, the turns it missed
while it was down.

---

## 6. Briefings

When you start a new session in a folder, conbus looks at recent conversations
in the same folder — across all your agents — and hands the new session a short
account of what they were doing, through the vendor's own context channel
(`additionalContext` for Claude Code and Codex, `additional_context` on Cursor's
`sessionStart`). It names at most **3** conversations and stays under about
**2,600 characters**; over that, it drops whole conversations rather than
cutting text mid-way.

A session is **not** briefed when:

| `doctor` says | Because |
| --- | --- |
| `silent/nothing_to_say` | nothing in that folder is recent or substantial enough |
| `silent/arrived_with_context` | the session opened with a long brief of its own |
| `silent/unattended` | a program is driving it |
| `silent/machine_authored` | the prompt says a program wrote it |
| `silent/no_workspace` | the vendor did not say which folder |
| `silent/workspace_too_broad` | the folder is `/` or your home directory |

To turn briefings off entirely, set `CONBUS_BRIEFING=off` in the environment
your agents run in. Recording continues.

A briefing is text from your own log placed into another session — which that
session's vendor then receives like anything else you type (section 11).

---

## 7. Hops

A hop moves the current conversation to another leg. conbus records the
decision in the log, opens or refreshes a **warm** session on the destination,
and brings you there.

### When conbus hops on its own

| Signal | Hop |
| --- | --- |
| A turn stopped for quota (`rate_limit`, `StopFailure`, a vendor's quota code) | Claude Code ↔ Codex; other legs overflow to Codex |
| The account reports its limit reached | the same |
| 80% or more used, with the reset more than 2 hours away | a planned handoff before the wall |
| A refusal for policy or authentication | **no hop** — that is not a quota problem |
| The destination is exhausted too | **wait** instead |
| The session is unattended (a program) | **no hop** |

The automatic hop is experimental in 0.2.8. It has fired on one real quota stop
(2026-09-27), and its next leg was ready 21.6 minutes after the decision, when
the limit had already lifted. `daemon.log` now says what each automatic hop
decided and how long it took.

### Hopping on purpose

```bash
conbus hop                # preview: where would the current conversation go?
conbus hop cursor         # preview a hop to Cursor
conbus go cursor          # do it
conbus hop --from 01a0c7f2 --to codex --apply   # a specific conversation
```

"The current conversation" is the one with the most recent turn on this Mac.
Pass `--from` with a bus id (or its first characters) to choose another.

You can also say it in the agent itself. A prompt that is **only** an order is
treated as `conbus go`:

- `switch to cursor`, `hop to codex`, `hand off to grok`, `go to opencode`,
  `transfer to claude`, `continue on codex`, `resume cursor`
- `이제 커서로 작업할거야`, `코덱스로 넘겨`, `클로드로 재개`

A sentence that merely mentions an agent while asking for something else is not
an order.

### Where a hop lands

- **Claude Code and Codex**: in a warm session conbus opened itself, never in a
  thread you opened. Codex.app opens on that thread (`codex://threads/…`), the
  Claude desktop app on that session (`claude://code/continue?session=…`), and a
  `conbus run` terminal resumes it in place.
- **Cursor**: `cursor -n <folder>` opens the project in a new Cursor window.
- **grok, opencode, agy**: in the terminal where `conbus run` is waiting. A hop
  made without a terminal — the recorder's own, on a quota stop — opens a new
  Terminal window instead.

### `conbus run`: one terminal, any agent

```bash
conbus run claude_code
```

starts the agent in the current terminal and stays as its parent. When a hop
happens, `conbus run` stops the current agent and starts the next one **in the
same terminal**, resuming the warm session. Legs: `claude_code` (the default),
`codex`, `cursor`, `agy`, `opencode`, `grok`.

A hop that nobody's `conbus run` picks up within **10 minutes** is discarded,
not followed later; `conbus run` says so when it starts. When it does pick one
up, it says which leg it is starting instead of the one you asked for.

### History

`conbus status` shows the routing rules, each leg's quota, and the last hops.
Hops you typed and hops a test staged are counted separately from the ones
that happened on their own (`excluded` in `doctor`).

---

## 8. Quota

```bash
conbus quota
```

| Leg | Where the number comes from |
| --- | --- |
| Codex | Codex's own app-server (`account/rateLimits/read`), for the whole account, including Codex.app |
| agy | `agy -p /usage`, once `agy models` shows a login |
| Claude Code | the `rate_limits` Claude Code gives its StatusLine command |
| Cursor, grok, opencode | nothing yet: their hooks carry no remaining-quota field |

**Claude's number is visible only from the terminal `claude`.** The desktop app
and SDK sessions never run a StatusLine, so a reading can be days old; after
90 minutes it is shown but no longer used for routing, and `quota` says how old
it is and when the StatusLine last ran. conbus does not read Claude's
credentials to ask for the number another way.

**agy is read only when it is signed in.** With no login, `agy -p` opens a
browser tab, and no time limit prevents that, so conbus runs `agy models` first.
If that reports no login, `quota`, `doctor` and `status` say `AUTH_REQUIRED`; if
it fails another way or runs out of time, they say `SKIP`. Either way `agy -p`
does not run and there is no reading, and a hop goes on without one. To sign in,
run `agy` in a terminal.

---

## 9. Recovering history

The recorder sees only what happened after it was installed. Earlier history
can be recovered from the agents' own records:

```bash
conbus backfill            # report what would be recovered
conbus backfill --apply    # write it
```

This reads Claude Code's transcripts (`~/.claude/projects`) and Codex's threads
(through Codex's own app-server), and files each session as a conversation
marked as recovered. A session the recorder already saw live is not duplicated.

`--apply` refuses while the recorder is running, because both would write to
the same file. Stop it, backfill, start it again:

```bash
launchctl bootout gui/$(id -u)/sh.conbus.daemon            # macOS
systemctl --user disable --now sh.conbus.daemon.service   # Linux
conbus backfill --apply
conbus install --apply     # starts the recorder again
```

**Missed turns** of live sessions — from times the recorder was down — are
filled in automatically when it starts, from Claude Code's transcripts, Codex's
threads and any chat export. The start path spends at most fifteen seconds on
it (`CONBUS_GAP_BUDGET_MS` overrides) so a slow vendor never holds the socket
closed; a fill the budget cuts short is remembered in `cache/gap-since` and
resumed on the next start, and `daemon.log` says so. `conbus backfill --gaps`
fills the same gaps on demand without a deadline, for Claude Code, across all
history; with `--apply` it too needs the recorder stopped. Deleting `cache/`
drops the remembered range — run `conbus backfill --gaps` after one.

**Claude.ai conversations** have no hooks in the desktop app. Export your data
from Claude.ai (Settings → Privacy → Export data) and drop the zip (or
`conversations.json`) into `~/.conbus/chat-export/`; the recorder picks it up.
`conbus backfill --chat-export PATH` reads one from anywhere and, with
`--apply` and the recorder stopped, writes it. Organisations using Claude's
Inference hooks can point them at `conbus hook --inference-listen`
(`127.0.0.1:8787` by default) to record chats as they happen. It takes
loopback addresses only: it authenticates nothing, so the HTTPS terminator in
front of it runs on the same machine.

---

## 10. Facts shared across checkouts

A fact is a short, confirmed statement about a project that every new session
there should know — "payments go through the queue, never directly".

```bash
conbus arena fact --workspace ~/src/shop --claim "payments go through the queue" \
  --anchor src/payments/queue.rs --type constraint
conbus arena link --from ~/src/shop --to ~/src/shop-admin
```

`--type` is one of `decision`, `constraint`, `state`, `pointer`, `open`.
Accepted facts appear in the briefing for that folder; `link` makes one
folder's facts visible in another.

---

## 11. Your data

### Where it lives

`$CONBUS_HOME` if set, otherwise `$XDG_STATE_HOME/conbus`, otherwise
`~/.conbus`:

| Path | Contents |
| --- | --- |
| `log/segment-*.jsonl` | the log: append-only JSON lines, in 8 MiB segments |
| `drops.jsonl` | events that could not be recorded, and why |
| `daemon.log` | the recorder's own messages, timestamped |
| `daemon.build` | which build the running recorder is |
| `conbus.sock` | the recorder's socket (mode `0600`) |
| `quota/` | the last quota readings |
| `takeover.json` | a hop waiting for `conbus run`, while one is |
| `facts.jsonl`, `links.jsonl` | section 10 |
| `policy.json` | your `PreToolUse` rules, if you wrote any (section 15) |
| `backup.json` | the last good `conbus backup`: where, when, how much |
| `chat-export/` | Claude.ai exports you dropped in |

Only your account can read any of it: the folder is `0700` and the log's
segments `0600`. Builds before 0.2.5 made them `0755` and `0644`, which other
accounts on the Mac could read; the recorder closes the folder when it starts,
and says so in `daemon.log` if it cannot — the log folder is closed either way.

The socket path must stay under 104 bytes; `doctor` tells you if a
`CONBUS_HOME` is too deep.

### How much is kept

`CONBUS_RETENTION`, set where the recorder runs:

| Value | Prompts and replies | Tool inputs and outputs |
| --- | --- | --- |
| *(unset)* | kept | not written — only their size |
| `full` | kept | kept (each body clipped at 16 KiB) |
| `encrypted_tools` | kept | sealed with AES-256-GCM under a key in your login Keychain; `conbus log --reveal` opens them. On Linux there is no OS key store and conbus keeps none of its own, so bodies are elided, not sealed |
| `ephemeral` | nothing is written | nothing is written |

At every level, a single body longer than 16 KiB is clipped in the log and
its full size noted.

The recorder runs under launchd on a Mac and systemd on Linux, so "where the
recorder runs" is its plist or its unit. `conbus install --apply` keeps what
you put there:

```bash
# macOS
/usr/libexec/PlistBuddy -c 'Add :EnvironmentVariables dict' \
  -c 'Add :EnvironmentVariables:CONBUS_RETENTION string encrypted_tools' \
  ~/Library/LaunchAgents/sh.conbus.daemon.plist

# Linux
systemctl --user edit sh.conbus.daemon.service   # add Environment=CONBUS_RETENTION=…

conbus install --apply   # reloads the recorder with it
```

The level applies from then on; what was written before stays as it was.

### What leaves your Mac

conbus has no server, no account and no telemetry. Three features put text
from your log into an agent session, which that agent's vendor then receives
like any other context:

- a **briefing** (section 6) — `CONBUS_BRIEFING=off` stops it;
- a **hop** (section 7), which opens the next leg with the conversation so far;
- `conbus summary --bus-id ID`, which asks the *other* leg's agent to summarise a
  conversation — only when you run it.

`conbus usage --otlp URL` sends counts — the `--shareable` rows, never text — to
a collector on this Mac that you name, and only when you run it (section 5).
Where they go from there is that collector's configuration.

`conbus backup --to FOLDER` writes a plain-text copy of the log's closed segments
to the folder you name, only when you run it. It refuses cloud-synced folders and
`~/Desktop` and `~/Documents`, and on a network share it says the copy leaves this
machine ([below](#backing-it-up-experimental)).

To read quota, conbus runs the agents' own CLIs (section 8); deciding a hop asks
`gh` about the folder's pull request.

### What conbus never touches

Vendor credentials and tokens. The agents' session files, except when
`backfill` runs and when the recorder fills gaps at start. Your `~/Downloads`
folder.

### Backing it up (experimental)

Until you make another, the log is the only copy. A dead disk, a lost Mac or a
stray `rm` loses all of it. `conbus doctor` says where you stand:

```text
backup    none: this log has one copy
```

```bash
conbus backup --to "/Volumes/My Disk"
conbus backup --verify --to "/Volumes/My Disk"
```

You run it, or your own scheduler does. Nothing in conbus ever will: a copy made
behind your back would outlive you deleting `~/.conbus` to remove your prompts.

- **What it copies.** Closed segments not yet there, into
  `<folder>/conbus-backup/log/`, and a `manifest.json` with each one's name, size
  and SHA-256. A rerun copies only what is new. The segment still being written
  is left until it rolls over at 8 MiB, so every run says how many records are
  newer than the backup. A log under 8 MiB has nothing closed yet: the run says
  so, and it still has one copy.
- **How it copies.** Each segment goes under a temporary name, is read back and
  compared, and only then gets its real name, so an interrupted run leaves no half
  file that looks complete. The next run removes what it left. On the log's own
  APFS volume the copy is a clone, which costs almost no space, and the run says
  so. Elsewhere it is a real copy.
- **`--verify`** rereads the whole backup and exits 1, naming each file that
  differs, is missing, or is not in the manifest. It detects damage, not tampering:
  there is no key, so anyone who can change a copy can change the manifest to match.
  A normal run does not read what is already there, so it will not notice a copy gone
  bad. To mend one, delete it from the backup and run `conbus backup`: if the log
  still has that segment, the copy comes back, checked against the manifest. A segment
  that has left the log stays in the backup, because a backup deletes nothing. If its
  copy is lost as well, those records are gone: each run names it, `doctor` counts it
  as lost, and `--verify` goes on naming it for as long as the manifest lists it.
- **A copy nothing can vouch for.** A file with a segment's name, no row in the
  manifest and no segment in the log to compare it with stays where it is, because it
  may be all that is left of those records. It is not added to the manifest, and each
  run and `--verify` go on naming it.
- **The last good run** is in `~/.conbus/backup.json`, with every folder a backup
  has gone to, which goes on record before the first copy lands there. `doctor` reads
  only that file, so it stays fast and says nothing about whether the disk is plugged
  in now:

  ```text
  backup    last ok 3h ago to /Volumes/My Disk (1,460 segments)
  ```

**What it refuses**, exiting 1. No flag overrides any of these.

- A folder inside the data root, or one that holds it.
- A cloud-synced folder: `~/Library/Mobile Documents`, `~/Library/CloudStorage`,
  `~/Dropbox`, `~/Dropbox (…)`, `~/Google Drive`, `~/OneDrive*`, including through a
  symlink. The log holds prompts and replies in plain text, and conbus promises they
  do not leave this machine. Encrypted cloud backup is not offered yet.
- `~/Desktop` and `~/Documents`, by name. On a Mac iCloud can sync them, and conbus
  cannot see that setting. Choose another folder, such as `~/conbus-backup`.
- A folder whose parent does not exist, or a folder directly under where this
  system mounts disks with no disk mounted there: `/Volumes` on a Mac, `/mnt`,
  `/media`, `/media/$USER` and `/run/media/$USER` on Linux. An unplugged disk fails,
  instead of creating a look-alike folder on another disk. So does any folder that
  held a backup at its last run and holds none now, which is what a mount folder looks
  like once its disk has gone. If you removed that backup on purpose, take the folder
  out of the destinations in `backup.json` first.
- A volume without room for the copy, checked before anything is copied.
- A log that is not the one in the backup: a closed segment whose size says it is
  not the copy, or a newest segment that is not its copy and perhaps more. Nothing is
  copied and nothing is overwritten. That is another log's backup, a damaged segment,
  or a new log started after a lost one.
- A `backup.json` that does not read, because it is the only list of where plain text
  went. Mend or delete it.

conbus knows only these names. It cannot see another sync tool, so choosing one sends
your prompts wherever that goes. A network share is not refused, because a NAS is a
reasonable place for a backup, but on a Mac the run and `doctor` say
`network share: this copy leaves this machine, in plain text`. On Linux conbus cannot
tell.

**What a backup is not.**

- **Private.** Prompts and replies are plain text at the destination, and so are tool
  inputs and outputs if the log was written under `CONBUS_RETENTION=full`. File modes
  protect nothing on an external disk, because macOS ignores ownership there and
  exFAT has no modes, so an unencrypted external disk exposes them to whoever holds
  it. Encrypt the disk.
- **A way to open sealed tool bodies elsewhere.** Under `encrypted_tools` they are
  sealed with a key in your login Keychain (`log-kek`), which a backup does not
  copy. On another machine those bodies stay unreadable; the rest of the log reads
  normally.
- **A second disk, unless it is one.** On the log's own volume, or another volume in
  the same APFS container, the run and `doctor` add
  `same disk: protects against deletion, not a failed disk`. A clone shares its blocks
  with the original. A separate partition of the same physical disk is not flagged, and
  fails with it.
- **Protection against a full disk.** A disk that fills up still loses new records.
- **Everything.** It holds the log only: not `policy.json`, `facts.jsonl`,
  `links.jsonl` or the rest of the root.

**Restoring.** There is no restore command. Paste the block for your system, with
your own folder on its third line. It stops at the first thing that fails, and only
its last line starts the recorder. On a new machine, do this before the first
`conbus install --apply`.

On a Mac:

```bash
(
  set -e
  disk="/Volumes/My Disk"
  conbus backup --verify --to "$disk"
  find "$disk/conbus-backup/log/" -name 'segment-*.jsonl' | grep . >/dev/null
  launchctl bootout "gui/$(id -u)/sh.conbus.daemon" || true
  report="$(conbus doctor || true)"
  printf '%s\n' "$report" | grep '^socket'
  printf '%s\n' "$report" | grep '^socket.*not answering' >/dev/null
  log="$(printf '%s\n' "$report" | sed -n 's/^log  *\(\/.*\)$/\1/p')"
  (umask 077 && mkdir -p "$log")
  find "$log" -name 'segment-*.jsonl' -empty -delete
  for copy in "$disk/conbus-backup/log/"segment-*.jsonl; do
    [ -e "$log/${copy##*/}" ] || cp "$copy" "$log/"
  done
  chmod 600 "$log/"segment-*.jsonl
  rm -rf "$log/../cache/meta"
  conbus backup --to "$disk"
  conbus install --apply
)
```

On Linux:

```bash
(
  set -e
  disk="/Volumes/My Disk"
  conbus backup --verify --to "$disk"
  find "$disk/conbus-backup/log/" -name 'segment-*.jsonl' | grep . >/dev/null
  systemctl --user disable --now sh.conbus.daemon.service || true
  report="$(conbus doctor || true)"
  printf '%s\n' "$report" | grep '^socket'
  printf '%s\n' "$report" | grep '^socket.*not answering' >/dev/null
  log="$(printf '%s\n' "$report" | sed -n 's/^log  *\(\/.*\)$/\1/p')"
  (umask 077 && mkdir -p "$log")
  find "$log" -name 'segment-*.jsonl' -empty -delete
  for copy in "$disk/conbus-backup/log/"segment-*.jsonl; do
    [ -e "$log/${copy##*/}" ] || cp "$copy" "$log/"
  done
  chmod 600 "$log/"segment-*.jsonl
  rm -rf "$log/../cache/meta"
  conbus backup --to "$disk"
  conbus install --apply
)
```

What each part is for:

- **`--verify` comes first,** while the recorder is still running, so a disk that is
  unplugged or misnamed stops the block with nothing changed.
- **The first `find` stops the block if the backup holds no segment,** again with
  nothing changed. A backup made before anything had closed passes `--verify`, which
  says there is nothing to restore, and the recorder must not be stopped for it.
- **The recorder has to stop.** A recorder left running after its log is deleted
  starts a new log, and a restore keeps any segment the log already has, so it would
  keep that new file over the real one. `conbus doctor` says
  `socket … not answering` once it has stopped. The block prints that line, and stops
  there if it says anything else. `launchctl` and `systemctl` complaining that nothing
  was running is fine.
- **The log's folder comes from `conbus doctor`,** so a root under `CONBUS_HOME` or
  `XDG_STATE_HOME` is restored where conbus reads it.
- **The second `find` removes empty segments,** which hold no records. An older
  `conbus doctor` makes one in an empty log folder, and the restore would keep it in
  place of the real one.
- **`cache/meta` goes** because a restore can keep a file's time, and that cache trusts
  a segment by its name, length and time. It is rebuilt on the next read.
- **The last `conbus backup` is the check that the restore worked.** It refuses if a
  closed segment is not the size of its copy, or if the newest segment is not its copy
  and perhaps more.

If the block stops at `--verify` or the first `find`, nothing has changed and the
recorder is as it was. If it stops later, the recorder it stopped stays stopped until
you start it, and `conbus install --apply` does that without restoring anything.

- **`--verify` says there is nothing to restore.** No segment had closed when that
  backup ran, so it holds no records. The block stops there. If the recorder is not
  running, `conbus install --apply` starts it.
- **`--verify` failed.** Nothing has changed. It names each bad file. While the log
  still has that segment, delete the copy from the backup and run `conbus backup`,
  which copies it again. If the log has lost it too, that copy is all there is. A
  damaged line only reads as unreadable and the rest of the segment survives, so you
  can still restore it: paste the block without its `--verify` line, and if its last
  check then refuses, run `conbus install --apply` yourself. That folder then goes on
  refusing that segment, so keep it and send later backups to another folder. A file
  `--verify` says nothing vouches for is restored too if you do that, and as the newest
  segment the recorder would write to it, so move it out of the backup first unless you
  know it is yours.
- **The `socket` line says `listening`.** A recorder is still running. Stop it and
  paste the block again.
- **The last check refused.** It names the segment. If it says `cannot put it right`,
  the backup's copy is damaged too and pasting again cannot help: run `conbus install
  --apply` yourself, keep this folder, and send later backups to another one. Otherwise,
  if the log's copy is damaged, delete that file from the log and paste the block again.
  If a recorder has started a new log there, see "Not over a new log" below.

And afterwards:

- **What comes back.** Everything that was closed when the backup ran. `conbus log
  --json` and `conbus usage --json` read exactly as they did over those records.
- **What does not.** The records that run called newer than the backup. After
  losing the whole log they are gone, which is up to one segment, plus whatever
  was written since that run. When the recorder starts, its gap fill (section 9)
  may bring some of those turns back from the agents' own transcripts, marked as
  recovered.
- **Over a partly missing log.** The same block. Only the missing segments are
  copied, and with the open segment still there, both reads come out exactly as
  they were before the loss.
- **An overlap.** A segment in both places is left as the log has it. No record can
  appear twice, because a segment has one name. Keeping the log's copy matters:
  after a restore the newest restored segment is the open one again and grows, and
  overwriting it would cut off what it gained. To replace a segment you know is
  damaged, delete it from the log first.
- **The next `conbus backup` to that folder carries on from there.** A restored
  segment that grew replaces its shorter copy once it closes, because the old copy is
  exactly how the new one starts. It cannot see a restore done by hand into the wrong
  folder: that shows only as every segment being `no longer in the log`.
- **Not over a new log.** If a recorder has already started a new log there, stop
  it and move that `log/` aside first. Two logs reuse the same segment names and
  sequence numbers, and conbus cannot merge them.

### Deleting it

`rm -rf ~/.conbus` removes everything conbus recorded. `uninstall` never does
this for you. Neither touches a backup, which is in a folder you named. `conbus
uninstall` prints every folder a backup went to, including one a run started and did
not finish, and the list goes with `~/.conbus`, so run it first and delete each
`conbus-backup` it names. On the log's own disk a clone keeps its blocks until it is
deleted too, so removing the log alone frees almost nothing.

---

### `conbus sync` (experimental)

`conbus sync` is the client for an invited service that receives one row of
counts per day — the same numbers `usage --shareable` prints, with no folder
names. No service exists yet; [RFD 0002](rfd/0002-metadata-sync.md) and
[RFD 0003](rfd/0003-security-design.md) are drafts. `conbus sync --join
INVITE` only shows the invite; with `--apply` it stores a device key in the
login Keychain. `conbus sync --off` deletes that key even when the service
cannot be reached. The log itself never leaves the machine.

## 12. Upgrading

Run the installer again, then restart the recorder:

```bash
curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
conbus install --apply
```

The second command changes no settings that are already registered; it
restarts the recorder on the new binary, which takes a few seconds. If `doctor`
shows `daemon … DIFFERS`, this step was skipped. A hook newer than the running
recorder is refused with "Restart the daemon" rather than half-understood.

The command unloads the recorder and loads it again (`bootout`, then
`bootstrap`, on a Mac; `daemon-reload` and `enable --now` on Linux) rather
than restarting it: macOS checks a replaced binary when the agent is loaded,
and refuses a plain restart of one once. The hooks go unrecorded for the few
seconds the recorder takes to start.

---

## 13. Uninstalling

```bash
conbus uninstall          # what would be removed
conbus uninstall --apply  # remove it
```

- Every hook entry that runs conbus comes out of every agent's settings, and
  nothing else does — your own hooks stay, including any in the same block.
- The StatusLine goes back to your own command.
- The opencode plugin, the grok hook file, the launchd agent or systemd user
  unit (after stopping the recorder) and `~/.local/bin/conbus` are deleted.
- Each rewritten settings file keeps a `…json.conbus-uninstall-backup`.
- Codex remembers approvals by position: if other hooks move up a place,
  uninstall says so, and Codex will ask you to approve them again.
- **Your log is kept.** Delete `~/.conbus` yourself if you want it gone.
- **So is every backup, in plain text.** Uninstall prints each folder a backup went to,
  from `backup.json`, and never touches them. Delete those yourself if you want them
  gone. Do it before `rm -rf ~/.conbus`, which takes that list with it. `conbus doctor`
  cannot help afterwards: uninstall has removed the binary, and it only ever named the
  last folder.

---

## 14. Troubleshooting

**`conbus: command not found`** — add `~/.local/bin` to your `PATH`:
`export PATH="$HOME/.local/bin:$PATH"` in your shell profile.

**macOS says the binary "cannot be opened"** — it was downloaded in a browser,
which marks it as quarantined. `xattr -d com.apple.quarantine conbus`, or
install with `install.sh`, which uses `curl`.

**`socket … not answering`** — the recorder is not running. `conbus install --apply`
starts it. If it keeps stopping, `~/.conbus/daemon.log` says why. Do not run
`conbus daemon` by hand while the launchd agent or systemd unit is loaded;
they compete for the socket.

**`socket … UNUSABLE — the socket path … is N bytes`** — set `CONBUS_HOME` to a
shorter directory.

**`hooks … CANNOT RUN`** — the registered binary is missing or broken. Run
`conbus install --apply` again.

**`codex … NOT TRUSTED` or `MODIFIED`** — start Codex and approve the hooks it
lists. `MODIFIED` also appears when another tool edits Codex's `hooks.json`.

**`drops` keeps growing** — look at the last reason `doctor` prints.
`no conversation for this session` on `SessionEnd` is normal for a session
that ended before its first prompt. `names no session` means an agent sent an
event without saying which session it belonged to; conbus drops those rather
than guess.

**`quota claude … StatusLine never ran`** — restart the terminal `claude` so it
loads the wrapped StatusLine. The desktop app never runs one (section 8).

**`takeover  stale → …`** — a hop was made while no `conbus run` was waiting.
The next `conbus run` discards it; nothing else to do.

**`nameless  <bus id> — N records…`** — a build before 0.1.0 filed events that
named no session under one shared conversation. 0.1.0 and later refuse such
events; the old records stay in the log, which is append-only, and this line
says what they are.

**A hop did not happen when a limit hit** — check `conbus status`: the rules
line, the destination's quota, and whether the session was a program
(unattended sessions never hop). `conbus hop` shows what it would decide now.

---

## 15. The local policy

`policy.json` in the data folder is your own list of `PreToolUse` decisions.
With no file, policy is off and every tool call passes through exactly as the
agent's vendor already decides. With one, the hook — not the recorder —
checks each call against the rules before the call runs, so a stopped
recorder cannot lift a deny.

```json
{
  "version": 1,
  "rules": [
    {
      "harness": "claude_code",
      "tool": "Bash",
      "input_contains": ["rm -rf /"],
      "decision": "deny",
      "reason": "no agent wipes this disk"
    }
  ]
}
```

- `harness` is optional. Leave it out and the rule applies to every agent
  that takes decisions at all — today Claude Code and Codex. Cursor's reply
  contract has no decision, so a Cursor call is never ruled on.
- `tool` is the payload's `tool_name`, spelled exactly; `"*"` is every tool.
- `input_contains` is any number of substrings; the rule matches when one of
  them appears in the tool input's compact JSON. Leave it out and every
  input matches.
- `decision` is `deny` or `ask`. There is no `allow` on purpose: a policy can
  tighten what runs, never lift a prompt the vendor would have shown.
- `reason` is shown to you, so it is required and cannot be empty.

Every rule is tried in order; any matching `deny` beats every matching `ask`,
and between equals the first rule's reason stands. A file that does not
parse, has the wrong version, or names an unknown field decides nothing —
`conbus doctor` says so, and `conbus policy check` prints the rules or the
error. `conbus policy test --harness claude_code --tool Bash --input
'{"command":"ls"}'` shows what the file would do with one call without
running it.

A decision is not recorded in the log; the tool request is, as it always was.

## 16. Claude Code profiles: a second account, switched by hand (experimental)

Claude Code lets you run more than one account side by side: a profile `NAME`
is the directory `$HOME/.claude-NAME`, and `CLAUDE_CONFIG_DIR` tells `claude`
which one it is. conbus supports exactly that — your own accounts, logged in
through the vendor's own flow, switched only when you say so.

```bash
# 1. Create the profile and log in — the vendor's own flow, not ours:
CLAUDE_CONFIG_DIR=$HOME/.claude-work claude
/login

# 2. Register the same hooks and StatusLine wrap into the profile's
#    settings.json:
conbus install --apply --profile work

# 3. Run the profile:
conbus run claude_code --profile work
```

A profile name is `[a-z0-9-]+`. If `$HOME/.claude-NAME` does not exist yet,
`install --profile` refuses and prints the login command — conbus never
creates the directory, never logs anyone in, and never reads anything but
`settings.json` inside it. `uninstall --profile NAME` takes the registration
back out of that file and leaves the machine's own settings alone.

Records from a profiled session carry `profile` in their source stamp, and
the profile's StatusLine readings land in their own file —
`conbus quota` shows them as `claude_code@NAME` — so routing reads the quota
of the account a conversation actually runs under. Backfill and gap fill
stay default-profile only; the vendor's transcripts there are the default
profile's.

**Switching is manual only.** The vendor's terms forbid circumventing its
usage limits, so conbus never switches profiles on a limit: no quota signal,
route or hop moves a conversation from one of your accounts to another, and a
hop back into Claude from a profiled run keeps the run's profile — it does
not resume the default profile's warm session, which the profile cannot see;
the fresh session is briefed from the checkout by the hooks instead. When a
conversation's last Claude leg ran under a profile, an automatic hop back to
`claude_code` would land on the *default* account, so the route waits instead
and says so — `conbus go claude_code` (your choice, on the default account)
or `conbus run claude_code --profile NAME`.

## 17. Reference

### Commands

| Command | |
| --- | --- |
| `conbus install [--apply] [--profile NAME]` | Register the hooks with every installed agent; reports unless `--apply`. `--profile` registers Claude Code in `$HOME/.claude-NAME` only |
| `conbus install --managed` | Print the hooks block for Claude Code's managed settings (JSON on stdout) and where it goes; writes nothing ([Managed Macs](fleet.md)) |
| `conbus uninstall [--apply] [--profile NAME]` | Take them all back out; the log is kept. `--profile` removes only `$HOME/.claude-NAME`'s registration |
| `conbus doctor` | What is wired up and what is not |
| `conbus status` | Routing rules, quota, hop history, cost |
| `conbus quota` | What each leg has left |
| `conbus policy check` | The effective `policy.json` rules, or why there are none (section 15) |
| `conbus policy test --harness H --tool T --input JSON` | What the rules would decide for one call |
| `conbus log [--json] [--bus-id ID] [--since TS] [--tail N] [--grep TEXT] [--reveal]` | Read the log |
| `conbus hop [HARNESS] [--from ID] [--to HARNESS] [--apply] [--demo]` | Preview, or with `--apply` make, a hop |
| `conbus go [HARNESS] [--from ID]` | `hop --apply` for the current conversation |
| `conbus run [HARNESS] [--profile NAME]` | Start an agent in this terminal; hops start the next one here. `--profile` runs `claude_code` under `$HOME/.claude-NAME` |
| `conbus backfill [--apply] [--gaps] [--chat-export PATH]` | Recover history |
| `conbus backup --to FOLDER` | Copy closed log segments to a folder you name, and read them back (experimental; section 11) |
| `conbus backup --verify --to FOLDER` | Recheck the whole backup; names any file that differs or is missing, and exits 1 (experimental) |
| `conbus summary --bus-id ID` | Ask the other leg's agent for a digest of a conversation |
| `conbus usage [--days N] [--since DAY] [--day DAY] [--json] [--shareable] [--otlp URL]` | What the agents did, counted; `--otlp` sends the `--shareable` rows to a collector on this Mac (section 5) |
| `conbus progress [--days N] [--since DAY] [--day DAY] [--workspace DIR] [--json]` | The same counts per folder, with the agents that worked there, the handovers between them and the commits `HEAD` gained (section 5) |
| `conbus verdict` | Score a four-week trial of the hop (below) |
| `conbus arena fact --workspace DIR --claim TEXT --anchor PATH [--type T]` | Accept a fact |
| `conbus arena link --from DIR --to DIR` | Share one folder's facts with another |
| `conbus daemon` | The recorder (launchd runs it) |
| `conbus hook [--harness NAME] [--statusline] [--inference-listen [ADDR]]` | The hook the agents call |
| `conbus --version` | Version and commit |

`conbus <command> --help` prints the usage.

### Environment

| Variable | Effect |
| --- | --- |
| `CONBUS_HOME` | Where conbus keeps its data (section 11). Name a folder of its own: conbus makes it `0700` |
| `XDG_STATE_HOME` | Used when `CONBUS_HOME` is unset: `$XDG_STATE_HOME/conbus` |
| `CONBUS_RETENTION` | `full`, `encrypted_tools`, `ephemeral` (section 11) |
| `CONBUS_BRIEFING` | `off` stops briefings |
| `CODEX_HOME` | Where Codex keeps its settings (default `~/.codex`) |
| `CONBUS_CLAUDE_BIN`, `CONBUS_CODEX_BIN`, `CONBUS_CURSOR_BIN`, `CONBUS_CURSOR_APP_BIN`, `CONBUS_GROK_BIN`, `CONBUS_OPENCODE_BIN`, `CONBUS_AGY_BIN` | Use a different binary for that agent |
| `CONBUS_GH` | Use a different `gh` |

### `conbus verdict`

conbus was built on a hypothesis: that people who use several agents hit limits
often enough for an automatic hop to matter. `verdict` scores that on your own
machine over 28 days of live use. It counts only hops that happened on their
own — never ones you typed or a test staged — and reads the result as: fewer
than 3 routing hops a week, no case for routing; a closed window with none at
all, inconclusive. Until 28 days have passed it says so and scores nothing.

---

## 18. Known limitations

- macOS and Linux only.
- On Linux there is no OS key store, so `encrypted_tools` elides tool bodies
  rather than sealing them — nothing is sealed under a key kept in a file.
- On Linux the release tarball is x86_64 only, and the recorder is a systemd
  user unit, not a launchd agent (`systemctl --user`; where no user manager
  answers, `conbus daemon` in the background does the same job).
- On Linux the hop window lands in the current TTY only: there is no
  osascript, so no new Terminal window opens on its own.
- Claude's quota is visible only from the terminal `claude` StatusLine.
- The automatic hop has not yet fired on a real quota stop.
- Some Cursor sessions send tool events without saying which session they
  belong to; conbus drops those instead of guessing.
- Codex hooks need your approval inside Codex.
- Releases before v0.2.6 are not notarized (section 14 for browser
  downloads). From v0.2.6 the binary is signed with a Developer ID and
  notarized.
- Cursor, grok and opencode do not report remaining quota, so conbus cannot
  plan a hop ahead of their limits.
- A backup leaves out the segment still being written, up to 8 MiB of the newest
  records (section 11).
