# conbus manual

This manual describes **conbus 0.2.1** on macOS. For the short version, read
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
- [15. Reference](#15-reference)
- [16. Known limitations](#16-known-limitations)

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

- **macOS** on Apple silicon or Intel. conbus uses launchd and the vendors'
  macOS apps; it does not run on Linux or Windows yet.
- **At least one agent, already signed in** through its own binary: Claude Code
  (`claude`), Codex (`codex` or Codex.app), Cursor, the Grok Build CLI (`grok`),
  or opencode. conbus never signs in for you and never reads their credentials.
- **`~/.local/bin` on your `PATH`**, or be ready to add it. The hooks call the
  binary by its absolute path, so they work either way; your shell needs it to
  find `conbus`.

While the repository is private, installing from a release also needs the
GitHub CLI signed in with access: `gh auth login`.

---

## 3. Install

### From a release

```bash
curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
```

The script:

1. downloads `conbus-macos-universal.tar.gz` from the latest release (falling
   back to `gh release download` when the anonymous download is refused);
2. checks it against its published SHA-256, and stops without installing
   anything if they differ;
3. places the binary at `~/.local/bin/conbus`, written beside and then renamed,
   so an agent in the middle of a hook is never cut off;
4. runs `conbus install`, which only **reports** what it would register.

Nothing in your agents' settings has changed at this point. Options:

| Variable | Effect |
| --- | --- |
| `CONBUS_VERSION=v0.2.1` | Install that release instead of the latest |
| `CONBUS_REPO=owner/name` | Install from a fork |
| `CONBUS_BASE_URL=…` | Download from a mirror, or `file:///path/to/dist` for a local build |

### From source

Rust 1.94 or newer:

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

For each agent **that is installed on this Mac** — recognised by its own
configuration directory — conbus merges one hook into that agent's settings.
Agents that are not installed are skipped; run `install --apply` again after
installing one.

| Agent | File | What is registered |
| --- | --- | --- |
| Claude Code | `~/.claude/settings.json` | 13 events, and the StatusLine (wrapped, so your own status line still shows) |
| Codex | `$CODEX_HOME/hooks.json` (default `~/.codex`) | 7 events |
| Cursor | `~/.cursor/hooks.json` | `sessionStart`, `beforeSubmitPrompt`, `stop`, `sessionEnd`, `afterAgentResponse` |
| grok | `~/.grok/hooks/conbus.json` | 9 events |
| opencode | `~/.config/opencode/plugins/conbus.js` | a plugin that forwards session and message events |

It also places the binary at `~/.local/bin/conbus` if it is not there yet, and
writes a launchd agent, `~/Library/LaunchAgents/sh.conbus.daemon.plist`, which
keeps the recorder running and starts it at login.

Every file it changes keeps a copy beside it (`…json.conbus-backup`), your own
hooks and settings stay exactly as they were — including their order — and a
file that is not valid JSON is reported and left untouched.

### Approving the hooks in Codex

Codex runs a new hook only after you approve it. Start Codex once; it lists the
hooks that are new or changed and asks whether to trust them. Until you do,
Codex hooks are registered and silently do nothing. conbus never writes that
approval for you — it is your consent, not conbus's.

---

## 4. Check that it works

1. Open any agent in a project folder and send one prompt.
2. Run `conbus log --tail 5`. The turn is there.
3. Run `conbus doctor`.

`doctor` prints one line per fact. The ones that matter first:

| Line | Healthy |
| --- | --- |
| `build` | `0.2.1 (<commit>)` |
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
```

**Counting it:** `conbus usage` folds the log into what your agents did, per
agent, over the last 7 days (`--days 30`, or `--since 2026-09-01`). Thirty days
on the machine that built it:

```text
usage     from 2026-08-25 (UTC) · 119745 live record(s) · not counted: backfilled 37559 · earlier 0 · repeated 9782
harness   claude_code — 2534 conversation(s): attended 211 · unattended 2260 · unknown 63
          2967 turn(s) · subagents 980
          stopped: limit 211 in 3 episode(s) · account 111 · vendor 3 · request 0 · other 1 · by a person 0
          26188 tool call(s): failed 510 · interrupted 0 · refused 46 · asks 244 · 18h in tools
harness   codex — 130 conversation(s): attended 9 · unattended 0 · unknown 121
          184 turn(s) · subagents 0
          stopped: limit 0 in 0 episode(s) · account 0 · vendor 0 · request 0 · other 0 · by a person 0
          1892 tool call(s): failed 0 · interrupted 0 · refused 0 · asks 1 · 0s in tools
failing   Bash 420 · Read 61 · Glob 4
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
- **repeated**: a second delivery of the same report — with some agents one
  event reaches the log twice. It is counted once.
- History recovered by `backfill` is not counted: it is not activity the
  recorder saw. Neither is cost: no hook carries tokens or prices.

`conbus usage --json` prints one row per day, agent, folder and attendance:
counts only, never a prompt, a reply or a tool body. `conbus usage --shareable`
prints the same rows with the folder removed — what a sync would send, if one
existed ([RFD 0002](rfd/0002-metadata-sync.md)). Nothing is sent anywhere; the
point is that you can read it first.

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

The automatic hop is experimental in 0.2.1: it has worked in probes and on
request, but has not yet been seen firing on a real quota stop.

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
| agy | `agy -p /usage` |
| Claude Code | the `rate_limits` Claude Code gives its StatusLine command |
| Cursor, grok, opencode | nothing yet: their hooks carry no remaining-quota field |

**Claude's number is visible only from the terminal `claude`.** The desktop app
and SDK sessions never run a StatusLine, so a reading can be days old; after
90 minutes it is shown but no longer used for routing, and `quota` says how old
it is and when the StatusLine last ran. conbus does not read Claude's
credentials to ask for the number another way.

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
launchctl bootout gui/$(id -u)/sh.conbus.daemon
conbus backfill --apply
conbus install --apply     # starts the recorder again
```

**Missed turns** of live sessions — from times the recorder was down — are
filled in automatically when it starts, from Claude Code's transcripts, Codex's
threads and any chat export. `conbus backfill --gaps` does it on demand for
Claude Code, across all history; with `--apply` it too needs the recorder
stopped.

**Claude.ai conversations** have no hooks in the desktop app. Export your data
from Claude.ai (Settings → Privacy → Export data) and drop the zip (or
`conversations.json`) into `~/.conbus/chat-export/`; the recorder picks it up.
`conbus backfill --chat-export PATH` reads one from anywhere and, with
`--apply` and the recorder stopped, writes it. Organisations using Claude's
Inference hooks can point them at `conbus hook --inference-listen`
(`127.0.0.1:8787` by default) to record chats as they happen.

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
| `chat-export/` | Claude.ai exports you dropped in |

The socket path must stay under 104 bytes; `doctor` tells you if a
`CONBUS_HOME` is too deep.

### How much is kept

`CONBUS_RETENTION`, set where the recorder runs:

| Value | Prompts and replies | Tool inputs and outputs |
| --- | --- | --- |
| *(unset)* | kept | not written — only their size |
| `full` | kept | kept (each body clipped at 16 KiB) |
| `encrypted_tools` | kept | sealed with AES-256-GCM under a key in your login Keychain; `conbus log --reveal` opens them |
| `ephemeral` | nothing is written | nothing is written |

At every level, a single body longer than 16 KiB is clipped in the log and
its full size noted.

### What leaves your Mac

conbus has no server, no account and no telemetry. Three features put text
from your log into an agent session, which that agent's vendor then receives
like any other context:

- a **briefing** (section 6) — `CONBUS_BRIEFING=off` stops it;
- a **hop** (section 7), which opens the next leg with the conversation so far;
- `conbus summary --bus-id ID`, which asks the *other* leg's agent to summarise a
  conversation — only when you run it.

To read quota, conbus runs the agents' own CLIs (section 8); deciding a hop asks
`gh` about the folder's pull request.

### What conbus never touches

Vendor credentials and tokens. The agents' session files, except when
`backfill` runs and when the recorder fills gaps at start. Your `~/Downloads`
folder.

### Deleting it

`rm -rf ~/.conbus` removes everything conbus recorded. `uninstall` never does
this for you.

---

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

---

## 13. Uninstalling

```bash
conbus uninstall          # what would be removed
conbus uninstall --apply  # remove it
```

- Every hook entry that runs conbus comes out of every agent's settings, and
  nothing else does — your own hooks stay, including any in the same block.
- The StatusLine goes back to your own command.
- The opencode plugin, the grok hook file, the launchd agent (after stopping
  the recorder) and `~/.local/bin/conbus` are deleted.
- Each rewritten settings file keeps a `…json.conbus-uninstall-backup`.
- Codex remembers approvals by position: if other hooks move up a place,
  uninstall says so, and Codex will ask you to approve them again.
- **Your log is kept.** Delete `~/.conbus` yourself if you want it gone.

---

## 14. Troubleshooting

**`conbus: command not found`** — add `~/.local/bin` to your `PATH`:
`export PATH="$HOME/.local/bin:$PATH"` in your shell profile.

**macOS says the binary "cannot be opened"** — it was downloaded in a browser,
which marks it as quarantined. `xattr -d com.apple.quarantine conbus`, or
install with `install.sh`, which uses `curl`.

**`socket … not answering`** — the recorder is not running. `conbus install --apply`
starts it. If it keeps stopping, `~/.conbus/daemon.log` says why. Do not run
`conbus daemon` by hand while the launchd agent is loaded; they compete for
the socket.

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

## 15. Reference

### Commands

| Command | |
| --- | --- |
| `conbus install [--apply]` | Register the hooks with every installed agent; reports unless `--apply` |
| `conbus uninstall [--apply]` | Take them all back out; the log is kept |
| `conbus doctor` | What is wired up and what is not |
| `conbus status` | Routing rules, quota, hop history, cost |
| `conbus quota` | What each leg has left |
| `conbus log [--json] [--bus-id ID] [--since TS] [--tail N] [--grep TEXT] [--reveal]` | Read the log |
| `conbus hop [HARNESS] [--from ID] [--to HARNESS] [--apply] [--demo]` | Preview, or with `--apply` make, a hop |
| `conbus go [HARNESS] [--from ID]` | `hop --apply` for the current conversation |
| `conbus run [HARNESS]` | Start an agent in this terminal; hops start the next one here |
| `conbus backfill [--apply] [--gaps] [--chat-export PATH]` | Recover history |
| `conbus summary --bus-id ID` | Ask the other leg's agent for a digest of a conversation |
| `conbus usage [--days N] [--since DAY] [--json] [--shareable]` | What the agents did, counted (section 5) |
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
| `CONBUS_HOME` | Where conbus keeps its data (section 11) |
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

## 16. Known limitations

- macOS only.
- Claude's quota is visible only from the terminal `claude` StatusLine.
- The automatic hop has not yet fired on a real quota stop.
- Some Cursor sessions send tool events without saying which session they
  belong to; conbus drops those instead of guessing.
- Codex hooks need your approval inside Codex.
- The binary is not notarized (section 14 for browser downloads).
- Cursor, grok and opencode do not report remaining quota, so conbus cannot
  plan a hop ahead of their limits.
