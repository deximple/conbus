# conbus security packet

For a security reviewer deciding whether conbus may run on developers' Macs
during a pilot. Every line can be checked against the code; the
[manual](manual.md) and [SECURITY.md](../SECURITY.md) have the detail. This
describes `main` from 0.2.5 (2026-09-24).

## In one paragraph

conbus records coding-agent sessions on the machine they run on. It is one
Rust binary at `~/.local/bin/conbus`, plus a launchd agent
(`sh.conbus.daemon`) that runs it as the person, with no root and no system
extension. It also adds hook entries to each agent's own settings. There is no
server, account or telemetry, and conbus makes no outbound connection of its
own. In a pilot, the only thing shared is the counts file described below, and
the participant sends it themselves.

## What it keeps, and where

Everything is under one root: `$CONBUS_HOME`, else `$XDG_STATE_HOME/conbus`,
else `~/.conbus`.

| Path | Holds | Mode |
| --- | --- | --- |
| the root | everything below | `0700` |
| `log/segment-*.jsonl` | prompts and replies; tool bodies only if `CONBUS_RETENTION` says so | directory `0700`, files `0600` |
| `conbus.sock` | the recorder's socket | `0600` |
| `facts.jsonl`, `links.jsonl`, `takeover.json`, `quota/`, `drops.jsonl`, `daemon.log`, `chat-export/` | derived state, and the recorder's own messages | inside the `0700` root |

- **Retention.** By default, prompts and replies are kept and tool inputs and
  outputs are not written, only their size. `CONBUS_RETENTION` changes that:
  - `full` keeps tool bodies.
  - `encrypted_tools` seals tool bodies with AES-256-GCM, under a key in the
    login Keychain (service `sh.conbus`, account `log-kek`).
  - `ephemeral` writes nothing.

  Any single body is clipped at 16 KiB.
- **Prompts and replies are plain text at rest.** File modes and FileVault are
  what protect them. Nothing is pruned: the log grows until the person
  deletes it.
- **Before 0.2.5** the root and log were created `0755`/`0644`, so other
  accounts on a shared Mac could read them. From 0.2.5 the recorder closes an
  existing root when it starts.
- The recorder turns off core dumps, and locks its memory where macOS allows.

## What leaves the Mac

conbus has no HTTP client. Text leaves only through programs the person already
runs:

- **Their agents.** A briefing, a hop and `conbus summary` put excerpts of the
  log into a session of the person's own agent. That agent's vendor receives
  them as context. `CONBUS_BRIEFING=off` stops briefings. Hops and summaries
  happen only when triggered.
- **Vendor CLIs and `gh`.** Quota comes from `agy -p /usage`,
  `codex app-server` and Claude Code's status line. To decide a hop, conbus
  asks `git` and `gh` about the folder's pull request. Each of these tools
  uses its own login, which conbus never reads.
- **Listeners.** The first is the Unix socket above. The second,
  `conbus hook --inference-listen`, is off unless started. It takes Anthropic
  Inference-hook frames over plain HTTP on `127.0.0.1:8787`, always answers
  allow, and discards the actor's email. **It does not authenticate
  requests**: anything that can reach the port can add records. Keep it on
  loopback, and authenticate at the organisation's HTTPS terminator.

## What it runs

| Program | When |
| --- | --- |
| `launchctl` | `install` and `uninstall` load and unload the agent |
| `security` | only with `encrypted_tools`: to create and read the sealing key |
| `/bin/sh -c` | to forward the person's own status-line command; `doctor` uses it to check that a registered hook command starts |
| `codex app-server`, `claude`, `agy`, and the CLI `conbus run` or `warm` starts | quota reads, hops, `summary`, `run`, `warm` |
| `open`, `osascript`, `cursor -n` | opening the window where a hop lands |
| `git`, `gh` | deciding a hop |
| `/usr/bin/unzip` | reading a Claude.ai export dropped in `chat-export/` |

## What it changes outside its root

`install --apply` works on these files:

- **Merges** hook entries into `~/.claude/settings.json`,
  `$CODEX_HOME/hooks.json` and `~/.cursor/hooks.json`.
- **Writes** `~/.grok/hooks/conbus.json`,
  `~/.config/opencode/plugins/conbus.js`,
  `~/.claude/conbus-statusline-forward` and
  `~/Library/LaunchAgents/sh.conbus.daemon.plist`.

`install` without `--apply` prints the change instead of making it. Before
each rewrite it keeps a `*.json.conbus-backup` copy. It replaces files
atomically and refuses a file it cannot parse. `uninstall --apply` removes
every entry that runs conbus and nothing else.

## What it never does

- Read, store or forward a vendor credential or token.
- Change a vendor binary.
- Write a trust record on the person's behalf: Codex asks about each hook.
- Pool accounts or quota.
- Write into a Codex thread the person opened.
- Read `~/Downloads`.
- Read a vendor's session files, except during `backfill` and the gap fill at
  recorder start.

## The counts a pilot shares

`conbus usage --json --shareable --day DAY` prints one row per UTC day,
harness and attendance.

- **A row has:** `day`, `harness`, `attendance`, `conversations`, and counts
  only. The counts are: turns; limit stops and episodes; account stops;
  vendor errors; request faults; other and user stops; subagents; tool calls,
  failures, interrupts and denials; permission asks; summed tool
  milliseconds; briefings; hops; repeated and recovered records.
- **A row never has:** a prompt, reply, error or reason text, path,
  workspace, command, tool or server name, id, hostname or username.

[RFD 0002](rfd/0002-metadata-sync.md) has the complete list. Tests fix the set
of keys, and check that a canary body never appears in a row. The participant
reads the file before sending it. No sync code exists, and none will before
[RFD 0003](rfd/0003-security-design.md) is accepted.

## Build and supply chain

- **Dependencies.** Rust 2024, with seven direct dependencies: `aes-gcm`,
  `getrandom`, `libc`, `serde`, `serde_json`, `time` and `uuid`.
  `Cargo.lock` lists 65 third-party packages, counting test-only and
  other-platform ones. Builds use `--locked`.
- **Releases.** GitHub Actions builds and tests each release on
  `macos-latest` from a tag, and publishes it with a SHA-256 that
  `install.sh` checks. The checksum comes from the same release, so it catches
  a damaged download but not a replaced release.
- **Signing.** The binary is ad-hoc signed and **not notarized**.
- **Licence.** Apache-2.0.

## Removing it

Run `conbus uninstall --apply`, then `rm -rf ~/.conbus`. With
`encrypted_tools`, also delete the key:
`security delete-generic-password -s sh.conbus -a log-kek`.

## Known gaps

- conbus does not encrypt prompts and replies at rest.
- Any process running as the person can write records to the socket. Such a
  process can read the log already.
- A hook runs on every agent turn with the person's privileges. Whoever can
  replace `~/.local/bin/conbus` therefore runs code in every session, as with
  any hook.
- It is not notarized, has had no third-party audit, and has no SBOM beyond
  `Cargo.lock`.

To report a vulnerability privately, see [SECURITY.md](../SECURITY.md).
