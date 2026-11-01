# conbus security packet

For a security reviewer deciding whether conbus may run on developers' Macs
during a pilot. Every line can be checked against the code; the
[manual](manual.md) and [SECURITY.md](../SECURITY.md) have the detail. This
describes `main` from 0.2.5 (2026-09-24).

## In one paragraph

conbus records coding-agent sessions on the machine they run on. It is one
Rust binary at `~/.local/bin/conbus`, plus a launchd agent
(`sh.conbus.daemon`) on macOS or a systemd user unit
(`sh.conbus.daemon.service`) on Linux that runs it as the person, with no
root and no system extension. It also adds hook entries to each agent's own settings. On a
managed Mac the organisation also places a copy the managed hooks name
([below](#on-a-managed-mac)); conbus itself writes no system file. There is no
server, account or telemetry. conbus opens one connection of its own, and only
when the person asks: `conbus usage --otlp URL` posts counts to a collector on
the same Mac. In a pilot, the only thing shared is the counts file described
below, and the participant sends it themselves.

## What it keeps, and where

Everything is under one root: `$CONBUS_HOME`, else `$XDG_STATE_HOME/conbus`,
else `~/.conbus`.

| Path | Holds | Mode |
| --- | --- | --- |
| the root | everything below | `0700` |
| `log/segment-*.jsonl` | prompts and replies; tool bodies only if `CONBUS_RETENTION` says so | directory `0700`, files `0600` |
| `conbus.sock` | the recorder's socket | `0600` |
| `facts.jsonl`, `links.jsonl`, `takeover.json`, `quota/`, `drops.jsonl`, `daemon.log`, `chat-export/` | derived state, and the recorder's own messages | inside the `0700` root |
| `policy.json` | the person's own `PreToolUse` rules, if they wrote any — off otherwise | inside the `0700` root |
| `cache/meta/` | a derived read cache: the log's records with tool bodies replaced by SHA-256 digests (prompts and replies as in the log); deleting it loses nothing | directories `0700`, files `0600` |
| `spool/` | hook requests waiting for a recorder that is restarting: retention applied before writing (tool bodies elided unless `full`), at most 1 MiB each and 16 MiB in all, deleted once ingested, never written where no recorder ever ran | directory `0700`, files `0600` |

- **Retention.** By default, prompts and replies are kept and tool inputs and
  outputs are not written, only their size. `CONBUS_RETENTION` changes that:
  - `full` keeps tool bodies.
  - `encrypted_tools` seals tool bodies with AES-256-GCM, under a key in the
    login Keychain (service `sh.conbus`, account `log-kek`). On Linux there is
    no OS key store and conbus keeps no file-based key, so `encrypted_tools`
    elides tool bodies instead of sealing them; `conbus doctor` says so.
  - `ephemeral` writes nothing.

  Any single body is clipped at 16 KiB.
- **Prompts and replies are plain text at rest.** File modes and FileVault are
  what protect them. Nothing is pruned: the log grows until the person
  deletes it.
- **Before 0.2.5** the root and log were created `0755`/`0644`, so other
  accounts on a shared Mac could read them. From 0.2.5, whatever creates the
  root creates it closed: the recorder, a hook, the status line, `install` or
  a hop. The recorder also closes an open root when it starts.
- **A root that cannot be closed** is still used, and the recorder says so in
  `daemon.log`. That happens when the root belongs to someone else or sits on
  a volume without modes. The log directory is closed either way.
- **Name a directory of its own for `CONBUS_HOME`.** conbus makes it `0700`,
  whatever else is in it.
- The recorder turns off core dumps, and locks its memory where the OS allows
  — `mlockall` is not implemented on macOS, so it reports rather than assumes.
- **The policy file is read, never written.** If `policy.json` exists, the
  hook process checks each `PreToolUse` payload against its rules itself —
  before and independently of the recorder — and answers `deny` or `ask` in
  the vendor's own hook output. A file that does not parse decides nothing
  (`doctor` says why), a missing file means policy is off, and there is no
  `allow`: the file can tighten a call, never lift a vendor's prompt. No rule
  comes from anywhere else — there is no server to push one.

## What leaves the Mac

Anything that leaves goes through a program the person already runs:

- **Counts, to the person's own collector.** conbus's one HTTP client,
  `conbus usage --otlp URL`, is off unless run with a URL. It sends the rows
  `--shareable` prints — counts only, no text, path or id — as OpenTelemetry
  metrics in one HTTP `POST`, to the address the person gives and no other.
  That address must be this Mac (`127.0.0.1`, `[::1]` or `localhost`):
  `https://`, any other host, and a URL with a user name or password are
  refused before a connection is made. The transport is plain HTTP/1.1 over
  loopback, so there is no TLS and no certificate to verify; the collector —
  the OpenTelemetry Collector, Grafana Alloy or the Datadog Agent, say —
  forwards the points under its own configuration and TLS. No dependency and
  no process is added: it is Rust's standard `TcpStream`, and nothing is
  started to send.
- **Their agents.** A briefing, a hop and `conbus summary` put excerpts of the
  log into a session of the person's own agent. That agent's vendor receives
  them as context. `CONBUS_BRIEFING=off` stops briefings. Hops and summaries
  happen only when triggered.
- **Vendor CLIs and `gh`.** Quota comes from `codex app-server`, Claude Code's
  status line, and for `agy` from `conbus quota`, a command the person runs: `agy
  models`, then `agy -p /usage` once that shows a login. `doctor`, `status` and the
  quota read behind a hop's decision use the reading it cached. A hop to `agy` that
  the person ordered and `conbus run` start `agy` itself as an interactive session.
  To decide a hop, conbus asks `git` and `gh` about the folder's pull request. Each
  of these tools uses its own login, which conbus never reads.
- **`conbus auth`.** It answers "whose sign-in page is this", and sends nothing
  anywhere of its own. `conbus auth` and `--html` run `codex app-server` for its
  limits read, `agy models` (never `-p`) and `gh auth status`. `--url` runs `lsof`
  and `ps`, for a callback port on this machine only. It reads no vendor config and
  no browser storage. The address it is given can hold a token: it stays in memory
  for that one run, only its scheme, host, callback parameters and `client` are
  decoded, and nothing of it is stored. No query value is printed but a callback's
  port and, as the URL's claim, a `client` name that is short and printable. An
  address on the command line is in shell history, so `conbus auth --url -` reads it
  from stdin, and the command warns otherwise. `--html` writes a `0600` page with a
  policy that lets it load nothing and reach nothing; vendor text in it is escaped.
- **Listeners.** The first is the Unix socket above. The second,
  `conbus hook --inference-listen`, is off unless started. It takes Anthropic
  Inference-hook frames over plain HTTP on `127.0.0.1:8787`, always answers
  allow, and discards the actor's email. **It does not authenticate
  requests**: anything that can reach the port can add records. It refuses
  any address that is not loopback, a name included when anything it
  resolves to is routable, so the organisation's HTTPS terminator — where
  authentication belongs — has to run on the same machine.

## What it runs

| Program | When |
| --- | --- |
| `launchctl` (macOS), `systemctl --user` (Linux) | `install` and `uninstall` load and unload the recorder's service |
| `security` | only with `encrypted_tools` on macOS: to create and read the sealing key |
| `/bin/sh -c` | to forward the person's own status-line command; `doctor` uses it to check that a registered hook command starts |
| `codex app-server`, `claude`, and the CLI `conbus run` or `warm` starts | quota reads, hops, `summary`, `run`, `warm`, `auth` |
| `agy` | `conbus quota` (`agy models`, then `agy -p /usage` once signed in); `conbus auth` (`agy models`, never `-p`); `conbus run`; a hop to it on a person's order, through `osascript`. Automatic overflow lands only on Codex or Claude |
| `opencode --version` | `doctor` and `install`: which OpenCode the plugin is written for |
| `lsof`, `ps` | `conbus auth --url`: walk what listens on a callback port on this machine |
| `open`, `osascript`, `cursor -n` | opening the window where a hop lands; `open` or `xdg-open` for `conbus auth --html --open`, on that flag only |
| `git`, `gh` | deciding a hop; `gh auth status` for `conbus auth` |
| `/usr/bin/unzip` | reading a Claude.ai export dropped in `chat-export/` |

## What it changes outside its root

`install --apply` works on these files:

- **Merges** hook entries into `~/.claude/settings.json`,
  `$CODEX_HOME/hooks.json` and `~/.cursor/hooks.json`.
- **Writes** `~/.grok/hooks/conbus.json`,
  `~/.config/opencode/plugins/conbus.js`,
  `~/.claude/conbus-statusline-forward` and the recorder's service file —
  `~/Library/LaunchAgents/sh.conbus.daemon.plist` on a Mac,
  `~/.config/systemd/user/sh.conbus.daemon.service` on Linux.

`install` without `--apply` prints the change instead of making it. Before
each rewrite it keeps a `*.json.conbus-backup` copy. It replaces files
atomically and refuses a file it cannot parse. `uninstall --apply` removes
every entry that runs conbus and nothing else.

### On a managed Mac

`install --managed` changes nothing, with or without `--apply`, and starts no
process: it prints the hooks block for Claude Code's managed settings, and
pointers for Codex and Cursor. The [fleet guide](fleet.md) asks the
organisation's own device tooling, with admin rights, to place these:

| What | Where | New process |
| --- | --- | --- |
| The binary the managed hooks name | a root-owned path every account can run, such as `/usr/local/bin/conbus` | none of its own: each agent starts `conbus hook` as the person, as it does the per-user hook |
| Claude Code's hooks block | `/Library/Application Support/ClaudeCode/managed-settings.d/conbus.json`, or the organisation's own `managed-settings.json` or `com.anthropic.claudecode` profile | none |
| Codex's managed hooks, optional | `[hooks]` in `/etc/codex/requirements.toml` or the `com.openai.codex` profile | none |
| A Login Items rule, optional | a `com.apple.servicemanagement` profile with a `Label` rule for `sh.conbus.daemon` | none |

Cursor's enterprise hooks file is not used: `install --apply` registers
Cursor per user. Each person still runs `install --apply` for the recorder,
which works on the per-user files above. The block holds only `hooks`: not
the StatusLine wrapper, which runs a command from the person's own
`~/.claude`, and not `allowManagedHooksOnly`, which stays the organisation's
to set.

## What it never does

- Read, store or forward a vendor credential or token. An address a person hands
  to `conbus auth --url` can hold one; it is not decoded, printed or kept.
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
- **Signing.** From v0.2.6, each release is signed with a Developer ID
  (team `4UN6WC576K`) and notarized by Apple.
  - The signature has the hardened runtime and one entitlement,
    `com.apple.security.automation.apple-events`.
  - A release tag is created as a draft (a `-rc.N` tag as a prerelease), so
    `latest` never points at the workflow's ad-hoc package;
    `scripts/sign-release.sh` on the operator's Mac replaces it and only then
    publishes the release (`docs/release.md`). Neither the certificate nor the
    notary credentials is in the repository or CI.
  - Each package carries GitHub build provenance and a CycloneDX bill of
    materials; `cargo deny` checks advisories, licences and sources on every
    release tag and weekly. `install.sh` on macOS refuses a release from this
    repository that is not signed with team `4UN6WC576K`.
  - 0.2.5 was ad-hoc signed on both halves. 0.2.4 and earlier shipped the
    Intel half unsigned.
- **Licence.** Apache-2.0.

## Removing it

Run `conbus uninstall --apply`, then `rm -rf ~/.conbus`. With
`encrypted_tools` on macOS, also delete the key:
`security delete-generic-password -s sh.conbus -a log-kek` (Linux keeps no
key; its tool bodies were elided). On a managed Mac,
remove the managed entries first and the system-wide binary last;
[the fleet guide](fleet.md#7-taking-it-back-out) gives the order.
`uninstall` touches neither.

## Known gaps

- conbus does not encrypt prompts and replies at rest.
- Any process running as the person can write records to the socket. Such a
  process can read the log already.
- `conbus usage --otlp` does not authenticate the collector: whatever listens
  on the port it is given receives the counts — on a shared Mac, possibly
  another account's process.
- A hook runs on every agent turn with the person's privileges. Whoever can
  replace `~/.local/bin/conbus` therefore runs code in every session, as with
  any hook. On a managed Mac the same holds for the system-wide binary the
  managed hooks name, across every account, so it belongs in a directory only
  root can write.
- It has had no third-party audit and has no SBOM beyond `Cargo.lock`.
- A release is ad-hoc signed and not notarized for the time between the
  workflow publishing it and the signed package replacing it.

To report a vulnerability privately, see [SECURITY.md](../SECURITY.md).
