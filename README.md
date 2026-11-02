# conbus

A local conversation bus for coding agents.

conbus records every conversation you have with Claude Code, Codex, Cursor,
grok and opencode into one log on your Mac, keyed so that a conversation keeps
one id while the harness under it changes. A new session in a checkout is
briefed on what recent sessions there were doing, and when a vendor stops a
turn for quota — or when you say so — the next leg picks the conversation up.
The vendor TUIs stay the window; conbus is the part underneath.

**1.0.0-rc.3 · macOS · Linux · Apache-2.0.** Local only: no hosted service, no
telemetry, and conbus never reads or stores vendor auth tokens. One harness
on one laptop does not need it.

## Why a local log, and not a sync tool

The record is an append-only JSONL stream. Git, a shared folder, and a remote
database of the text each handle that stream badly. conbus keeps the stream on
this Mac.

| Approach | What happens to the stream | What 1.0 does |
| --- | --- | --- |
| Git or dotfiles | High-frequency JSONL collides, and prompts land in the repository history. | Segments stay in `~/.conbus/log`. They are the source of truth (`crates/conbus-core/src/segment.rs`). |
| A file sync | The copy does not know where a record ends, and it is a copy of the prompts. | The log directory is mode `0700` and each segment is `0600`. conbus does not upload them. |
| A remote database of the text | Prompts leave the machine, and revoking an API token does not delete the local log. | There is no hosted bus. Uninstall leaves the log where it is; `rm -rf ~/.conbus` removes it. |

Counts, never the text, are a separate contract.
[RFD 0002](docs/rfd/0002-metadata-sync.md) names the rows a sync may send: one
row per device, UTC day, harness and attendance.
[RFD 0003](docs/rfd/0003-security-design.md) names device signing and
`conbus sync --off` as the leave command. Both documents stay drafts. The
client ships in 0.2.9; **there is still no service to join.** Until a pilot
asks, the rows go nowhere:

```bash
conbus usage --shareable                       # print those rows locally
conbus usage --otlp http://127.0.0.1:4318      # counts only, to a collector on this Mac
conbus sync --join INVITE                      # preview an invite; writes nothing
```

## Architecture

Hooks append to local segments. Briefing, hop and usage read those segments
back. A sync service is not part of this picture.

```text
Claude Code     Codex     Cursor     grok     opencode
      \           |          |         |         /
       conbus hook   (absolute path, one registration per vendor)
                           |
                           v
                recorder   launchd agent sh.conbus.daemon
                           |
                           v
                ~/.conbus/log/segment-NNNNNNNN.jsonl
                append-only, mode 0600, torn tail healed on open
                           |
             +-------------+-------------+
             |             |             |
          briefing        hop          usage
       next session    next harness     counts
```

A briefing, a hop, and `conbus summary` can place an excerpt into a vendor
session. That vendor then receives it like any other context.
`CONBUS_BRIEFING=off` stops the briefing. `conbus summary` asks the other
vendor, never the one whose conversation it is digesting.

`CONBUS_RETENTION=encrypted_tools` seals tool bodies with a key in the login
Keychain. That is the retention key. The device signing key in RFD 0003 is a
different key, minted only by `conbus sync --join INVITE --apply`.

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
```

The script downloads the binary for your OS — the universal macOS one or
`conbus-linux-x86_64` — from the latest release, checks its SHA-256, places it
at `~/.local/bin/conbus`, and shows what it would register. **Nothing in your
vendor settings changes yet.** Then:

```bash
conbus install --apply   # register the hooks, start the recorder
conbus doctor            # what is wired up, and what is not
```

`install --apply` merges one hook into each vendor it finds — `~/.claude/settings.json`
(and the StatusLine, wrapped so yours still shows), `~/.codex/hooks.json`,
`~/.cursor/hooks.json`, `~/.grok/hooks/conbus.json`, an opencode plugin — keeps a
`.conbus-backup` of every file it changes, and starts the recorder as a launchd
agent (`sh.conbus.daemon`) on a Mac or a systemd user unit
(`~/.config/systemd/user/sh.conbus.daemon.service`) on Linux. Where no
`systemctl --user` answers — a container, a remote shell — install still writes
everything and prints the one command that runs `conbus daemon` by hand. Your
own hooks and settings stay exactly as they were.

**Codex asks first.** Codex runs a new hook only after you approve it: start
Codex once and trust the hooks it lists. conbus never writes that trust record
for you.

From source instead (Rust 1.99+):

```bash
git clone https://github.com/deximple/conbus && cd conbus
cargo build --release -p conbus
./target/release/conbus install --apply
```

## First five minutes

1. Open any agent in a checkout and send one prompt.
2. `conbus log --tail 5` — the turn is in the log.
3. `conbus doctor` — `last` shows that turn; `drops` should stay flat.
4. `conbus hop --demo` — a recorded failover, with no vendor window opened.

## Daily use

```bash
conbus run claude_code   # start a leg; a hop execs the next one in this terminal
conbus go cursor         # hop the current conversation now
conbus hop               # preview where the next hop would go
conbus status            # routing rules, quota, hop history, cost
conbus usage             # what the agents did this week: stops, failures, permission asks
conbus progress          # the same, per folder: which agents worked there, and the commits
conbus log --bus-id ID   # one conversation, across every harness it touched
```

A chat line that is only an order — `이제 커서로 작업할거야`, "switch to cursor" —
is treated like `conbus go cursor`.

## What is recorded, and where it goes

- **Where.** `~/.conbus/log` (or `$XDG_STATE_HOME/conbus`, or `$CONBUS_HOME`):
  append-only JSONL. `conbus log` reads it back.
- **What.** By default prompts, replies, tool names and outcomes, permission
  asks and quota stops. **Tool inputs and outputs are not written.**
  `CONBUS_RETENTION` changes that: `full` keeps tool bodies,
  `encrypted_tools` seals them with a key in your login Keychain, and
  `ephemeral` writes nothing at all.
- **What leaves your Mac.** conbus has no server and no telemetry. Three
  features put excerpts of your local log into a vendor session, which that
  vendor then receives like any other context:
  - the **briefing** a new session gets — `CONBUS_BRIEFING=off` stops it;
  - a **hop**, which opens the next leg with the conversation so far;
  - `conbus summary`, which asks the *other* vendor to digest a conversation,
    only when you run it.

  `conbus usage --otlp URL` sends counts, never text, to an OpenTelemetry
  collector on this Mac that you name — only when you run it.

  `conbus sync` sends the closed days' counts, never prompts, to the origin a
  joined invite names — and nothing until `conbus sync --join INVITE --apply`
  joins this machine. `conbus sync --off` forgets the credential and leaves
  the log where it is.

  To read quota, conbus asks the vendor CLIs you already use (`codex
  app-server`, `agy`); deciding a hop asks `gh` for the checkout's pull request.
- **What conbus never reads.** Vendor auth tokens and credentials. Vendor
  session files are read by `conbus backfill` when you run it, and by the daemon
  when it starts, to fill in turns that happened while it was not running.

## Take it back out

```bash
conbus uninstall          # what would be removed
conbus uninstall --apply  # remove it
```

Every hook entry that runs conbus comes out of every vendor file — nothing
else does — the StatusLine goes back to yours, and the plugin, launchd agent
and binary are deleted. **Your log is kept**; uninstall prints where it is,
and `rm -rf ~/.conbus` removes it if you want it gone.

## Limits

- macOS only.
- Claude's remaining quota is visible only through the terminal `claude`
  StatusLine; the desktop app and SDK sessions never send one.
- The automatic hop is experimental. It has fired on one organic quota stop
  (2026-09-27), and its next leg arrived after the limit had already lifted.
  Probes and requests aside, that is the only one so far.
- From v0.2.6 the release binary is signed with a Developer ID and notarized
  by Apple, so a copy downloaded in a browser opens without
  `xattr -d com.apple.quarantine`. Releases before that still need it.
  `install.sh` uses `curl`, which does not quarantine either way.

## Commands

| Command | What it does |
| --- | --- |
| `conbus install [--apply]` | Register the hooks with every vendor found; reports unless told to write |
| `conbus uninstall [--apply]` | Take them all back out; the log is kept |
| `conbus doctor` | Install and daemon health |
| `conbus auth` | Asks Codex, agy and gh whether they are signed in (conbus itself needs no login); `conbus auth --url -` reads a URL from stdin and says whose page it is, or that it cannot tell (experimental) |
| `conbus status` | Routing rules, quota, hop history, cost |
| `conbus quota` | What each leg has left |
| `conbus log [FILTERS]` | Read the conversation log back |
| `conbus log index` | Build the derived read cache under `<root>/cache/` (`CONBUS_NO_CACHE=1` reads without it) |
| `conbus usage [--days N] [--since DAY] [--day DAY] [--json] [--shareable] [--otlp URL]` | What the agents did, counted: stops, tool failures, permission asks; `--otlp` sends the counts to a collector on this Mac |
| `conbus progress [--days N] [--since DAY] [--day DAY] [--workspace DIR] [--json]` | The same counts per folder: the agents that worked there, the handovers between them, the commits `HEAD` gained |
| `conbus hop [HARNESS]` | Preview the next leg (`--apply` records it) |
| `conbus go [HARNESS]` | Hop the current conversation |
| `conbus run [HARNESS]` | Start a leg in this terminal; hops exec the next one |
| `conbus backfill [--apply]` | Recover history from before conbus was installed |
| `conbus summary --bus-id ID` | A model digest from the other vendor (experimental) |
| `conbus verdict` | Score a four-week trial; an open window is not a verdict (experimental) |
| `conbus sync [--join INVITE [--apply]] [--off]` | Send closed days' counts to the origin a joined invite names; no host exists yet |
| `conbus policy check` · `conbus policy test` | The local `PreToolUse` rules in `policy.json` (experimental, off without the file) |
| `conbus arena fact \| link` | Accept a fact, or link two checkouts |
| `conbus daemon` · `conbus hook` | The recorder, and the hook it registers |
| `conbus --version` | Version and the commit it was built from |

`conbus <command> --help` prints the full usage; the [manual](docs/manual.md) explains each one.

## Documentation

Operational guides and the decisions behind them:

- [Manual](docs/manual.md) — install, the recorder, hop, upgrade, uninstall.
- [매뉴얼 (한국어)](docs/manual.ko.md) · [운영자 매뉴얼 (한국어)](docs/beginner-manual.md)
- [RFD 0001](docs/rfd/0001-hook-contract.md) — the hook contract the binary registers. Field names come from `crates/conbus/tests/fixtures/`.
- [Support matrix](docs/support-matrix.md) — which vendor versions were checked live, and what each one sends. [Stability](docs/stability.md) — what 1.x keeps. [Log format](docs/log-format.md).
- [RFD 0002](docs/rfd/0002-metadata-sync.md) — draft. Which usage rows a sync may send. The client ships in 1.0; there is no service.
- [RFD 0003](docs/rfd/0003-security-design.md) — draft. Device keys and `conbus sync --off`. It is accepted only together with RFD 0002, and neither has been accepted. The client side ships in 1.0.
- [Changelog](CHANGELOG.md) · [Security](SECURITY.md) · [Releasing](docs/release.md) · [Managed Macs](docs/fleet.md) · [Hop walkthrough](docs/hop-demo.md) · [Fact schema](docs/fact-schema.md) · [License](LICENSE)

conbus is a local tool. There is no hosted bus, account or price
([docs/saas-hypothesis.md](docs/saas-hypothesis.md)).

## 한국어

1.0은 한 대의 Mac에서 Claude Code, Codex, Cursor, grok, opencode의 대화를
로컬 JSONL 세그먼트로 남기고, 쿼터로 멈추거나 직접 말했을 때 다음 하네스로
넘기는 도구입니다. 호스티드 동기화 서비스는 없습니다.

설치는 위의 `install.sh`와 `conbus install --apply`가 전부입니다.
`conbus sync`는 1.0에 있지만 join할 서비스가 아직 없습니다. 참여한 뒤에는
닫힌 날의 횟수만 초대에 적힌 origin으로 나가고, 프롬프트는 로그에 남습니다.
운영 절차는 [한국어 매뉴얼](docs/manual.ko.md)에 있습니다.
