# Changelog

All notable changes to conbus. Versions follow [Semantic Versioning](https://semver.org/);
until 1.0 a minor version may change the command line or the log format, and
the entry says so when it does.

## [Unreleased]

### Fixed

- An upgrade leaves the hooks without a recorder for a few seconds, not ~14.
  macOS refuses the first start of a replaced binary once (a Background Task
  Management launch constraint: the build is ad-hoc signed, so every version is
  a new identity), and launchd's default then waited ten seconds before trying
  again. The launchd agent now sets `ThrottleInterval` to two seconds. When the
  agent's file changed, `conbus install --apply` unloads and reloads it
  (`bootout`, then `bootstrap`), because `kickstart -k` keeps the definition
  launchd already read — run it once after upgrading to this version.

## [0.2.1] — 2026-09-23

### Fixed

- A Grok event is recorded once. Grok runs the hooks in Claude Code's
  settings file as well as its own, so with both registered every Grok prompt,
  tool call and turn end reached the log twice. The copy that arrives through
  Claude Code's registration is now answered and not recorded, as long as
  `~/.grok/hooks/conbus.json` carries that event. Records already in the log
  are left as they are.

## [0.2.0] — 2026-09-23

A ledger of what the agents did, counted — the first local piece of the plan in
`docs/saas-strategy.md`. The command line gains one command; the log format
does not change.

### Added

- `conbus usage`: what the agents did, counted, per harness over the last 7 days
  (`--days N`, `--since DAY`). Conversations split by whether a person was
  there; turns and why they stopped; subagent runs; tool calls that failed, that
  you stopped, that were refused; permission asks; the tools that fail most and
  the folders with the most turns. `--json` prints one row per day, harness,
  folder and attendance — counts only, never a body. A report that reached the
  log twice — on the machine that built this, every Grok event did, and some
  Cursor ones — is counted once and named in the header as `repeated`. Cost is
  named as absent: no hook carries tokens or prices. A `--since` that is not a
  day like `2026-09-01` is refused rather than read as an empty window.
- `conbus usage` files a stop under what you would do about it: a usage limit,
  the account (authentication, billing, a hold), the vendor (overloaded, a
  server error), the request, or a reason this version does not recognise — not
  the router's split into quota and everything else, which put an overloaded
  vendor under quota and 111 authentication failures under "policy". Limit stops
  less than an hour apart on one harness are one episode: on the machine that
  built it, 211 limit stops in 30 days were 3 episodes.
- `conbus usage --shareable`: the rows a sync would send under RFD 0002 — the
  same counts with no folder field at all, rows that differed only by folder
  merged. Nothing is sent; it prints, so the payload can be read before any sync
  exists.

## [0.1.1] — 2026-09-23

### Fixed

- `install` registers only the vendors this machine has. It used to create
  `~/.codex`, `~/.grok`, `~/.cursor` and `~/.config/opencode` for someone who
  had only Claude Code, which `uninstall` could not tell were its own to
  remove. A vendor installed later is picked up by running `install --apply`
  again, and `doctor` says "not installed here" instead of telling you to run
  an install that would skip it.

## [0.1.0] — 2026-09-23

The first release someone other than its author can install, check, and take
back out. macOS only.

### What conbus is at 0.1.0

- **A recorder for coding agents.** One local log of every conversation across
  Claude Code, Codex, Cursor, grok and opencode — prompts, replies, tool calls,
  permission asks, quota stops — keyed so a conversation keeps one id while the
  harness under it changes. Append-only JSONL in `~/.conbus/log`. Tool result
  bodies are not written by default.
- **A briefing.** A new session in a checkout is told, through the vendor's own
  context channel, what recent conversations there were doing.
- **A hop** (experimental). When a vendor stops a turn for quota, or when you
  say so (`conbus go cursor`), the next leg continues under the same id; under
  `conbus run` it takes over the same terminal.
- **Backfill** of history from before the recorder was installed, from each
  vendor's official transcripts.
- **`conbus doctor`** says what is wired up and what is not, and
  **`conbus verdict`** scores a four-week trial honestly.

### Added

- `conbus uninstall [--apply]` removes every hook, file and agent `install`
  wrote, puts the StatusLine back, and never touches the log.
- Release builds: one universal macOS binary with its SHA-256, and `install.sh`,
  which checks the checksum and changes no vendor settings until you run
  `conbus install --apply`.
- A CI gate — fmt, clippy with warnings as errors, and the whole suite — on
  every pull request.
- `conbus --version` names the version as well as the commit.
- `doctor` names a conversation an older build keyed on an empty session id,
  shows a waiting takeover and whether it is stale, and reports the age of the
  Claude quota reading and of the last StatusLine run separately.

### Fixed

- A hook payload that names no session is dropped instead of being filed under
  one conversation that every nameless session shares.
- A takeover nobody took expires after ten minutes instead of redirecting the
  next `conbus run`, possibly into another project's thread.
- A data directory too deep for a Unix socket says so and names `CONBUS_HOME`
  as the fix, instead of the daemon dying with "path must be shorter than
  SUN_LEN".
- `install` keeps the key order of your settings files and the other fields
  on your StatusLine.
- The daemon starts serving in about two seconds instead of about twelve (71
  after a reboot, measured on a machine with 6,190 transcripts): filling
  turns missed while it was down now reads only what changed after the log's
  newest record, and no longer passes over the whole log once per file.

### Known limitations

- macOS only (launchd, AF_UNIX path limits, the vendor apps it lands in).
- Codex runs a hook only after you approve it inside Codex. conbus never writes
  that trust record; until you approve, Codex hooks are registered and silent.
- Claude's quota is visible only through the terminal `claude` StatusLine. The
  desktop app and SDK sessions never send one, and conbus does not read vendor
  tokens to fetch it another way.
- The hop has fired in probes and on request, not yet on an organic quota stop
  on a hooked leg. `conbus verdict` refuses to score a window without one.
- The binary is not notarized. `install.sh` downloads with `curl`, which does
  not quarantine; a copy downloaded in a browser needs
  `xattr -d com.apple.quarantine conbus` before macOS will run it.

[0.1.1]: https://github.com/deximple/conbus/releases/tag/v0.1.1
[0.1.0]: https://github.com/deximple/conbus/releases/tag/v0.1.0
