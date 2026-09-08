# Conbus

A conversation bus for coding agents.

One conversation keeps its identity while the harness underneath it changes. The bus owns the id, the log, and the hop. The vendor TUI stays the window.

## Install

macOS, with `claude` and `codex` already logged in through those binaries.

> The repository is private today, so the clone below works only for someone
> who has been given access. Making it public is an operator decision.

```bash
git clone https://github.com/deximple/conbus
cd conbus
cargo build --release -p conbus
./target/release/conbus install --apply
conbus doctor
conbus hop --demo
```

`install --apply` places `~/.local/bin/conbus` (beside, then `mv`), merges hooks into every vendor it finds — Claude Code, Codex, Cursor, grok, opencode — wraps Claude's StatusLine so the bus can read that quota, and writes `~/Library/LaunchAgents/sh.conbus.daemon.plist`. Put `~/.local/bin` on `PATH` if `conbus` is not found. It does not write Codex `trusted_hash`. Trust the hooks inside Codex, then `conbus run claude_code`.

## Daily

```bash
conbus run claude_code
```

Quota or `StopFailure` hops automatically. A chat line that is only an order (`이제 커서로 작업할거야`) or `conbus go cursor` is a directed hop. Preview: `conbus hop cursor`. Cursor opens the checkout with official `cursor -n`.

## Commands

| Command | What it does |
| --- | --- |
| `conbus doctor` | Install and daemon health |
| `conbus status` | Routing rules, quota, hop history, cost |
| `conbus quota` | What each leg has left |
| `conbus hop [HARNESS]` | Preview the next leg |
| `conbus go [HARNESS]` | Apply that hop |
| `conbus run [HARNESS]` | Same TTY; execs the next leg |
| `conbus verdict` | 4-week dogfood score; an open window is not a kill |
| `conbus summary --bus-id ID` | Opposite-leg digest; never the protected leg |
| `conbus log [FILTERS]` | Read the conversation log back |
| `conbus install [--apply]` | Register the hooks; reports unless told to write |
| `conbus backfill [--apply]` | Recover history the recorder was not running for |
| `conbus arena fact \| link` | Accept a fact, or link two checkouts |
| `conbus daemon` · `conbus hook` | The recorder, and the hook it registers |
| `conbus --version` | Which build this is |

`conbus <command> --help` prints the full usage, including the flags each one takes.

## Docs

First-time path: [beginner manual](docs/beginner-manual.md). Hop walkthrough: [hop demo](docs/hop-demo.md). Hook contract: [RFD 0001](docs/rfd/0001-hook-contract.md). Facts: [fact schema](docs/fact-schema.md). Public name: [conbus.sh](docs/www/index.html). License: [Apache-2.0](LICENSE).

Local first. No hosted dependency, no telemetry, and the bus never reads or stores vendor auth tokens.
