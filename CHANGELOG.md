# Changelog

All notable changes to conbus. Versions follow [Semantic Versioning](https://semver.org/);
until 1.0 a minor version may change the command line or the log format, and
the entry says so when it does.

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
