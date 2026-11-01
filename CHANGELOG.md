# Changelog

All notable changes to conbus. Versions follow [Semantic Versioning](https://semver.org/);
until 1.0 a minor version may change the command line or the log format, and
the entry says so when it does.

## [Unreleased]

### Fixed

- A second `conbus daemon` no longer writes anything before it gives up. It used
  to overwrite the live daemon's stamp, fill gaps and drain the spool, and only
  then lose the socket, which could leave a duplicate `seq` on disk. A daemon now
  takes a lock on `daemon.lock` first, waits up to five seconds for the recorder
  already there to go, and otherwise exits having written nothing. A start-up repair
  that outlives its daemon holds that lock until it ends, so the next daemon waits
  for it rather than writing beside it. A repair that never returns gives up after
  two minutes, and the daemon serves without it.

## [1.0.0-rc.3] — 2026-11-01

### Fixed

- `quota`, `doctor`, `status` and `hop` no longer open a browser tab when `agy`
  is not signed in. `agy -p` starts an OAuth flow with no login, and neither a
  deadline nor the lack of a terminal stops it, so they now run `agy models`
  first. `quota`, `doctor` and `status` report `AUTH_REQUIRED` or `SKIP` instead
  of a reading, and a hop goes on without one. Signing in by running `agy` is
  unchanged. `scripts/canary.sh`, `bench-corpus.sh` and `shadow-replay.sh` also
  keep older builds away from an `agy` with no login, and
  `scripts/check-no-browser.sh` checks the real `agy`.

## [1.0.0-rc.2] — 2026-11-01

### Fixed

- A start-up gap fill the time budget cut short is now remembered in
  `cache/gap-since` and resumed on the next start, instead of being lost to a
  newer `newest - margin`; the start budget defaults to fifteen seconds, and a
  Codex `sessions/` layout the build cannot read falls back to `thread/list`
  instead of silently stopping.

## [1.0.0-rc.1] — 2026-11-01

### Added

- `usage` rows and the OTLP pipe now count turn tokens where a vendor's hook
  reports them — `tokens_input`, `tokens_output`, `tokens_cache_read`,
  `tokens_cache_write`, today Cursor's `afterAgentResponse` only — and the
  `--shareable` rows carry them per RFD 0002's amendment.
- Reads that never print a tool body now run against a derived cache of
  digested records under `<root>/cache/meta/` — tool bodies are 93% of the
  log's bytes, so `usage`, `doctor`, `verdict`, `progress`, `hop`, the
  daemon's restore, and `log --tail` stop reading them. `conbus log index`
  builds the cache explicitly; `CONBUS_NO_CACHE=1` reads the segments
  directly. Deleting `cache/` loses nothing.
- Claude Code profiles (experimental): a second account under
  `CLAUDE_CONFIG_DIR`, switched only by hand — no limit signal, router rule
  or hop ever moves a conversation between profiles.
- conbus builds and records on Linux — x86_64 and aarch64 — with the recorder
  as a systemd user unit; where there is no OS key store, tool bodies are
  elided and `sync --join --apply` says so rather than writing one to a file.
  `install.sh` picks the tarball by `uname -m`.
- `conbus policy`: a local `policy.json` turns into `PreToolUse` decisions,
  off by default. The decision is made in the hook process, so a down
  recorder cannot lift a deny; a file that does not parse decides nothing
  and `doctor` says so. `conbus policy check` lists the effective rules and
  `conbus policy test` runs them against one call.

### Fixed

- A recorder restart no longer drops hooks: a hook that cannot reach the
  daemon writes its request to `<root>/spool/` (consent comes from the
  daemon's stamp and its retention), the next daemon drains it through the
  same writer, and a re-delivered `hook_id` counts once in usage.

### Changed

- `docs/log-format.md` documents a log line, the event kinds, bodies, and the
  rules for change: additions only in 1.x, older records always read, newer
  ones reported rather than guessed at, and what a rollback reads.
- `docs/stability.md` says what 1.x keeps — commands, exit codes, machine
  output keys, OTLP names, the log format, the data root, the hook contract —
  and what stays experimental; `--help` marks the experimental surfaces.
- Building from source needs Rust 1.99.1, pinned in `rust-toolchain.toml`;
  CI reads the same file.

### Fixed

- Grok 1.0.71: a turn that ends without completing (`StopCancelled`: a
  person interrupting, a declined permission, a turn limit) is recorded as an
  interrupted turn, and the extra `Stop` Grok fires after the session ends is
  no longer recorded as a second turn end. `install --apply` registers
  `StopCancelled`.
- OpenCode 2.x is recorded again. It refuses the plugin 1.x loads, so nothing
  reached the log from OpenCode after it moved to 2.0. `conbus install` now
  writes the plugin the installed OpenCode loads (asking `opencode --version`),
  `install --apply` replaces the other one, and `doctor` names a mismatch.

### Security

- Releases: a tag without a hyphen is created as a draft and becomes
  `latest` only after `scripts/sign-release.sh` has replaced the package with
  the signed one; a `-rc.N` tag is a prerelease. Packages carry build
  provenance and a CycloneDX bill of materials, `cargo deny` runs on release
  tags and weekly, and `install.sh` on macOS refuses a release from this
  repository that is not signed with conbus's Developer ID.
- `conbus hook --inference-listen` refuses an address that is not loopback.
  The listener authenticates nothing, and it used to bind whatever address it
  was given.
- `conbus sync` no longer lets a `.curlrc` change how it sends: curl runs with
  `-q` first, HTTPS and TLS 1.2 or newer only, no redirects, and the body is
  sent byte for byte as signed (`data-binary`). Before, a `.curlrc` line such
  as `insecure` or `proxy` applied to every send.

## [0.2.9] — 2026-11-01

`conbus sync`, the device side of the draft sync contract. There is still no
host: nothing joins, and nothing leaves this Mac until an invite's `--apply`
says so. Prompts stay in the log; only closed days' counts can be sent.

### Added

- `conbus sync --join INVITE` prints what the invite asks and writes nothing.
  With `--apply` it mints an Ed25519 device key, keeps the seed only in the
  login Keychain, and records the invite's settings in `sync.json`.
- `conbus sync` sends the closed UTC days not yet sent — the rows
  `conbus usage --shareable` prints — signed, bounded, and only to the origin
  the invite named. `conbus sync --off` leaves: it deletes the device key and
  the sync state even when the service cannot be reached, and the log is kept.

## [0.2.8] — 2026-10-09

The log, by folder. `conbus progress` shows, for each checkout, which agents
worked there, how much, what handed work between them, and the commits its
`HEAD` gained. Tool bodies can now be kept sealed without cost:
`encrypted_tools` no longer starts a Keychain process per body, and an
upgrade no longer drops the level you set. Run `conbus install --apply`
after upgrading.

### Added

- `conbus progress`: the `usage` counts regrouped by the folder the work was
  in — which agents worked there, how much, what handed work between them, and
  how many commits the folder's `HEAD` gained in the window. A checkout and its
  worktrees are one place. `--workspace DIR` keeps one folder, `--json` prints
  one folder per line. Counts only, like `usage`.

### Fixed

- `conbus install --apply` rewrote the recorder's launchd plist from its
  template, dropping any `EnvironmentVariables` an operator had added — the
  one place a launchd-run recorder can be given `CONBUS_RETENTION` — so an
  upgrade put the log back to its default level without a word. The
  environment is now carried over, and the manual says how to set it.
- `CONBUS_RETENTION=encrypted_tools` read the sealing key from the Keychain
  once per tool body, starting a `security` process for every tool input and
  output the recorder wrote, with no bound on how long it could take. The key
  is now read once per process, and `security` is given five seconds.
- `scripts/sign-release.sh` waits up to ten minutes for Gatekeeper to see a
  ticket Apple has just issued. Apple publishes it a few minutes after
  accepting; v0.2.7's check straight after acceptance failed, and passed on
  its own about two and a half minutes later.

## [0.2.7] — 2026-09-28

A recorder that stays small, and hops that say what they did. The recorder no
longer holds the log in memory. On a log of 298k records its footprint drops
from 946 MB to 235 MB, peak included, and it no longer grows with every record.
The first organic hop happened on 2026-09-27 and reached its next leg 21.6
minutes late. The GitHub and git calls on that path now give up after 5 s, and
every automatic hop writes what it decided and how long it took. This is the
first release signed with a Developer ID and notarized from the start. Run
`conbus install --apply` after upgrading.

### Fixed (recorder memory)

- The recorder no longer holds the log in memory. It read the whole log at
  start to rebuild its bindings, then kept those records as long as it ran,
  locked into RAM with the rest of its state: on a log of 298k records a
  946 MB footprint, growing with every record. The start-up gap fill and split
  repair, which do need the whole log, now run in a short-lived child
  (`conbus daemon --prepare`) whose memory goes back to the system when it
  exits. The recorder then folds its state from one streaming read. On the
  same log its footprint is 235 MB, peak included, and it starts in about 3 s.
  `daemon.log` says where the repair ran and how long it took.

### Fixed (hops)

- A hop is not held up by GitHub. Deciding one asks `git` and `gh` about the
  checkout for the handoff's freshness line, and neither call had a limit; each
  now gets 5 s and is marked stale after that. The first organic hop, on
  2026-09-27, reached its Codex thread 21.6 minutes after the decision, with
  those calls on its path.
- Every automatic `hop --apply` now writes its route and how long it took to
  `daemon.log`, whether it hopped, waited or failed. A successful one used to
  leave no line at all.

### Changed

- v0.2.6's release package is signed with a Developer ID and notarized by
  Apple. The binary, `CDHash 402c2287…`, is otherwise the one the workflow
  built. A copy downloaded in a browser opens without removing the
  quarantine, and a device-management rule can require the team identifier
  `4UN6WC576K`. Later releases go through `scripts/sign-release.sh` after the
  workflow publishes them.

- The pipe was checked through the Datadog Agent 7.83.2 as well. Datadog kept
  a closed day sent 2.4 hours after it ended, with every value equal to the
  `--shareable` row, and dropped one sent 26 hours after. RFD 0002 now says to
  send each day within a couple of hours of 00:00 UTC, rather than citing a
  one-hour limit.

### Fixed

- `scripts/sign-release.sh` checked a notarized command-line binary with
  Gatekeeper's app assessment, which rejects anything that is not an app. It
  now uses the assessment for other files, and requires "Notarized Developer
  ID" and the `=notarized` code requirement. A run after a failure finds
  Apple's ticket by code hash instead of submitting again.

## [0.2.6] — 2026-09-24

Release tooling and checks; the recorder does what 0.2.5 did. The pipe was
checked against a real OpenTelemetry Collector and Grafana Alloy, and every
point they kept equalled the `--shareable` row. `scripts/sign-release.sh`
signs a published release with a Developer ID and has Apple notarize it.
conbus's own tests no longer reach the recorder of the person running them.
Upgrading is the usual `conbus install --apply`.

### Added

- `tests/otlp_collector.rs`, an opt-in check of `usage --otlp` against a
  real OpenTelemetry Collector, which runs when `CONBUS_OTELCOL` names one.
  Against otelcol 0.161.0 every point the collector kept equalled the row
  `--shareable` printed, and a wrong path or a stopped collector exited
  non-zero. Grafana Alloy 1.19.2 forwarded every point unchanged too.
- `scripts/sign-release.sh` replaces a published release's package with one
  signed with a Developer ID and notarized by Apple: hardened runtime, a
  secure timestamp, and the one entitlement `scripts/conbus.entitlements`
  explains. No secret goes into the repository or CI; the operator's keychain
  holds the certificate and the notary profile. `CONBUS_SIGN_IDENTITY=-`
  rehearses it ad hoc. conbus has been run under the hardened runtime:
  recorder, hook, `doctor` and `usage`, with core dumps off and memory locked.

### Fixed

- conbus's own tests no longer reach the recorder of the person running them.
  Two Inference-listener tests went through the real data root: every
  `cargo test` sent their fixture frame to the running recorder, which
  logged it as a Chat turn (session `22222222-…`, request `req_abc123`), and
  wrote a drop and a `claude_chat` field-name file into `~/.conbus`. And the
  `quota` and `doctor` tests ran the machine's real `agy` and
  `codex app-server`. The listener now resolves the root once and hands it
  down, tests give it one of their own, and every test that runs `conbus`
  names its data root and vendor binaries. Records already in a developer's
  log stay, as everything in the log does; they are one conversation.

## [0.2.5] — 2026-09-24

Ready for a pilot's security review and a managed fleet. The log is readable
by its owner only, both halves of the release binary are signed,
`install --managed` prints the hooks for Claude Code's managed settings, and
`usage --otlp` sends the shareable counts to a collector on this Mac. The
reviewer's page is [`docs/security-packet.md`](docs/security-packet.md), and
[RFD 0003](docs/rfd/0003-security-design.md) drafts the sync's security before
any sync code. After upgrading, run `conbus install --apply`: the recorder
closes `~/.conbus` when it starts.

### Added

- `conbus install --managed` prints, and never writes, the hooks block an
  administrator adds to Claude Code's managed settings on Macs whose policy
  sets `allowManagedHooksOnly` — which blocks the per-user hooks
  `install --apply` registers, so conbus recorded nothing there. stdout is that
  JSON alone: the same events and command `install --apply` registers, naming
  the binary that was run. stderr says where it goes on macOS, how it merges
  with the organisation's own hooks, why the StatusLine wrapper stays out, and
  where Codex and Cursor take managed hooks. [docs/fleet.md](docs/fleet.md)
  covers the rest of a managed deployment: the binary, the per-user recorder,
  approving it as a managed login item, and removal.
- `conbus usage --otlp URL`: the counts, into the monitoring a team already
  runs. It sends exactly the rows `--shareable` prints for the window, as
  OTLP/HTTP JSON metrics — `conbus.turns`, `conbus.stops` by `kind`,
  `conbus.tool.failures` and the rest, tabled in RFD 0002 — to a collector on
  this Mac, such as the OpenTelemetry Collector, Grafana Alloy or the Datadog
  Agent on `127.0.0.1:4318`, which forwards them. A spike: one plain-HTTP
  `POST` to a loopback address, no retry and no new dependency. `https://` and
  other hosts are refused before a connection is made, and a send that fails
  exits non-zero. The unit is a closed `--day`, sent once: the points are
  deltas, so a day sent twice counts twice.

### Fixed

- The log is readable by its owner only. The data root was made with the
  default umask, and a macOS home directory is 0755, so every other account on
  the Mac could read `~/.conbus/log` — prompts and replies included. The root
  and the log directory are now 0700 and new segments 0600, whichever part of
  conbus makes them first, and the daemon closes a root an older build left
  open when it starts, so upgrading and running `conbus install --apply` is
  enough. A root it cannot close is used anyway and named in `daemon.log`.
- The Intel half of the release binary is signed. `lipo` kept the arm64
  slice's linker signature and left the x86_64 one unsigned; packaging now
  signs the universal binary ad hoc and refuses one that is not signed.

## [0.2.4] — 2026-09-23

Counts an operator can trust: recovered turns, one stop per Cursor turn, one
turn per OpenCode prompt, and a closed day. After upgrading, run
`conbus install --apply` and restart OpenCode, which loads plugins at startup.

### Added

- `conbus usage --day DAY`: one closed UTC day, and nothing after it — the unit
  RFD 0002 sends once a day.

### Changed

- `conbus usage` counts recovered records by when they happened and marks them
  `recovered`: turns gap fill wrote while the recorder was down, and
  conversations `backfill` brought in — Codex.app's threads have no hook, and
  2,442 of their records from the last 30 days were in the log and in no report.
  A recovered copy of a session the hooks also recorded is left out
  (`recorded twice`), and so is a run that declared itself a probe. The header
  names what it set aside, and only what it set aside.

### Fixed

- A Cursor turn that reported several stops counts its most serious one.
  Cursor writes a turn's end from `afterAgentResponse`, `stop` and sometimes
  `sessionEnd`, and its `generation_id` names one turn, so one exhausted turn
  could count as a limit and an `error`.
- A Cursor reply that mentions a limit code is a reply, not a quota stop.
  `afterAgentResponse` carries the agent's own words, and any reply containing
  `RESOURCE_EXHAUSTED` — explaining a gRPC error, say — was filed as Cursor
  refusing the turn. A response is now a limit only when it is one short line
  ending in the code, the way Cursor's error bubble reads
  (`request failed: PRO_USER_USAGE_LIMIT`). Status and error fields are read as
  before.
- OpenCode: one prompt is one turn, carrying what the person typed, and the
  agent's reply never becomes a prompt. The plugin sent a `turn_started` each
  time OpenCode published the person's message again — after every agent step —
  and forwarded every text part without asking whose it was: 61 turns for about
  8 prompts in a week. `install --apply` now replaces a conbus plugin this
  build did not write, instead of calling any conbus plugin registered, which
  had kept the fix from ever reaching a machine; and the plugin no longer takes
  OpenCode down when the hook closes its input unread.

## [0.2.3] — 2026-09-23

### Fixed

- `conbus install --apply` always reloads the recorder — `bootout`, then
  `bootstrap` — instead of restarting it with `kickstart -k`. A bootstrap makes
  macOS check the replaced binary, so an upgrade's first start is no longer
  refused: on the 0.2.2 deploy the new daemon started 0.3 s after `install`,
  and the hooks were unrecorded only for the recorder's own ~5 s start, with no
  record dropped. 0.2.2 reloaded only when the launchd file changed, which it
  will not on the next upgrade. If launchd will not load the file, `install`
  now says so and exits non-zero instead of carrying on.

## [0.2.2] — 2026-09-23

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
