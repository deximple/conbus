# conbus

A local conversation bus for coding agents.

conbus records every conversation you have with Claude Code, Codex, Cursor,
grok and opencode into one log on your Mac, keyed so that a conversation keeps
one id while the harness under it changes. A new session in a checkout is
briefed on what recent sessions there were doing, and when a vendor stops a
turn for quota — or when you say so — the next leg picks the conversation up.
The vendor TUIs stay the window; conbus is the part underneath.

**0.1.1 · macOS · Apache-2.0.** Local only: no hosted service, no telemetry,
and conbus never reads or stores vendor auth tokens.

> The repository is private today. Until it is public, installing from a
> release works for people who have been given access and are signed in with
> `gh auth login`. Making it public is an operator decision
> ([docs/release.md](docs/release.md)).

## Install

```bash
curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
```

The script downloads the universal macOS binary from the latest release,
checks its SHA-256, places it at `~/.local/bin/conbus`, and shows what it would
register. **Nothing in your vendor settings changes yet.** Then:

```bash
conbus install --apply   # register the hooks, start the recorder
conbus doctor            # what is wired up, and what is not
```

`install --apply` merges one hook into each vendor it finds — `~/.claude/settings.json`
(and the StatusLine, wrapped so yours still shows), `~/.codex/hooks.json`,
`~/.cursor/hooks.json`, `~/.grok/hooks/conbus.json`, an opencode plugin — keeps a
`.conbus-backup` of every file it changes, and starts a launchd agent
(`sh.conbus.daemon`). Your own hooks and settings stay exactly as they were.

**Codex asks first.** Codex runs a new hook only after you approve it: start
Codex once and trust the hooks it lists. conbus never writes that trust record
for you.

From source instead (Rust 1.94+):

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
- The automatic hop is experimental: it has fired in probes and on request, not
  yet on an organic quota stop on a hooked leg.
- The binary is not notarized. `install.sh` uses `curl`, which does not
  quarantine; a copy downloaded in a browser needs
  `xattr -d com.apple.quarantine conbus` first.

## Commands

| Command | What it does |
| --- | --- |
| `conbus install [--apply]` | Register the hooks with every vendor found; reports unless told to write |
| `conbus uninstall [--apply]` | Take them all back out; the log is kept |
| `conbus doctor` | Install and daemon health |
| `conbus status` | Routing rules, quota, hop history, cost |
| `conbus quota` | What each leg has left |
| `conbus log [FILTERS]` | Read the conversation log back |
| `conbus usage [--days N] [--since DAY] [--json] [--shareable]` | What the agents did, counted: stops, tool failures, permission asks |
| `conbus hop [HARNESS]` | Preview the next leg (`--apply` records it) |
| `conbus go [HARNESS]` | Hop the current conversation |
| `conbus run [HARNESS]` | Start a leg in this terminal; hops exec the next one |
| `conbus backfill [--apply]` | Recover history from before conbus was installed |
| `conbus summary --bus-id ID` | A model digest from the other vendor |
| `conbus verdict` | Score a four-week trial; an open window is not a verdict |
| `conbus arena fact \| link` | Accept a fact, or link two checkouts |
| `conbus daemon` · `conbus hook` | The recorder, and the hook it registers |
| `conbus --version` | Version and the commit it was built from |

`conbus <command> --help` prints the full usage; the [manual](docs/manual.md) explains each one.

## More

[Manual](docs/manual.md) · [Changelog](CHANGELOG.md) · [Security](SECURITY.md) · [Releasing](docs/release.md) ·
[Hop walkthrough](docs/hop-demo.md) · [Hook contract](docs/rfd/0001-hook-contract.md) ·
[Fact schema](docs/fact-schema.md) · [매뉴얼 (한국어)](docs/manual.ko.md) · [운영자 매뉴얼 (한국어)](docs/beginner-manual.md) ·
[License](LICENSE)

conbus is a local tool. There is no hosted bus, account or price
([docs/saas-hypothesis.md](docs/saas-hypothesis.md)).
