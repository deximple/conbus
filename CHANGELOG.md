# Changelog

All notable changes to conbus. Versions follow [Semantic Versioning](https://semver.org/);
until 1.0 a minor version may change the command line or the log format, and
the entry says so when it does.

## [Unreleased]

## [1.0.0-rc.4] — 2026-11-03

The candidate the 1.0 canary runs on. Most of it keeps records through trouble: a
full disk, a restart, a second recorder, a signal, a hook that arrives at the wrong
moment and a fill beside a start could each lose a record without a word or write it
twice. Now the record is kept or the loss is said, except in the cases under Known
limits. Gap fill no longer copies a parent conversation into its fork. Two
experimental commands are new: `conbus backup`, a second copy of the log made by
hand, and `conbus auth`, which says whose sign-in page just opened. Run
`conbus install --apply` after upgrading.

### Added

- `conbus backup --to FOLDER` (experimental) copies closed log segments that are
  not yet there into `FOLDER/conbus-backup/log/`, with a manifest of each one's
  name, size and SHA-256, and says how many records are newer than the backup.
  Each copy is read back before it gets its real name. On the same APFS volume
  it is a clone, which costs almost no space, and it says so.
  `conbus backup --verify --to FOLDER` rechecks the whole backup and names any
  file that differs or is missing. It runs only when a person or their own
  scheduler runs it: nothing in conbus does, because a hidden copy would outlive
  someone deleting `~/.conbus` to remove their prompts.
- `backup` refuses a folder inside the data root or holding it, a cloud-synced
  folder (also through a symlink), `~/Desktop` and `~/Documents`, a folder whose
  parent is missing, a folder under where this system mounts disks with no disk
  mounted, one that held a backup and holds none now, a volume without room, and a
  log that does not match what the folder holds. There is no override. Prompts and
  replies are plain text at the destination, and the key that opens sealed tool
  bodies is not copied. The first run to a folder says so, and manual section 11
  has that and the restore, which is a documented block to paste rather than a
  command.
- `conbus doctor` has a `backup` line, read from `<root>/backup.json` alone: when
  the last good run was, where to and how many segments, whether that is the
  log's own disk or a network share, or `none: this log has one copy`.
  `conbus uninstall` prints every folder a backup went to, from a list in
  `backup.json` that gets each folder before the first copy lands there, because
  those copies are plain text and nothing else says where they are.
- `conbus auth` (experimental): for when a sign-in tab opens and you should not have
  to guess whose it is. It asks Codex, agy and gh whether they are signed in, says
  which vendors it did not ask, and lists what conbus runs with nobody at the
  keyboard; conbus itself needs no login, and recording needs none.
  `pbpaste | conbus auth --url -` names a page from its scheme and host and, when it
  calls back to a port on this machine, walks every process listening there. It says
  `yes` only when a conbus command is above what listens and the page belongs to a
  vendor conbus starts itself, and `no` only when the chain goes up to an app or a
  person's session with no conbus in it, with what such a walk cannot see. Otherwise
  it says `cannot tell` and what would tell: a listener hanging straight from the root
  or a service manager, an unknown page, and anything under `conbus run`, which is
  your own agent, all read that way. `--url URL` works too, and warns when the URL has
  a query, because that is now in shell history. It reads no vendor config. It prints
  no query value but a callback's port and, as the URL's claim, a plain `client` name.
  `--html` writes the table and a paste box as one offline page. Its banner speaks
  only for what conbus needs: yellow names a needed sign-in that is missing, grey
  says `conbus could not tell for: …` when a vendor gave no answer conbus could read,
  which is no reason to sign in, or that nothing could be asked, and green says
  nothing it needs is signed out. The line under it sorts the vendors into asked,
  could not tell, not installed and not asked. Both manuals say what each one means.
- `conbus backfill --gaps --include-older-lines` (experimental). `--gaps` prints how
  many lines it set aside as older than their file, and this flag fills from them
  too. A transcript moved by `cp`, `rsync -a` or across volumes has a new creation
  time, so nothing else could fill it. Those lines may be another conversation's
  copies: on real transcripts the flag wrote a fork's parent turns into the fork, so
  look at a dry run first. A start never does this.
- `conbus doctor` has a `gap fill` line for each range still held: since when, why,
  and the file or thread that holds it. `conbus backfill --gaps` prints every reason
  it did not read something, and `daemon.log` names each file.
- Release packages carry `THIRD_PARTY_NOTICES.txt`, the license text of every crate
  built into the binary. They went out without it, though BSD-3-Clause asks for it
  in a binary distribution and MIT for a copy of its notice.

### Changed

- `doctor`, `status` and the quota read behind a hop's decision no longer run
  `agy`. They read what `conbus quota`, a command a person ran, cached. `doctor` and
  `status` show the reading with its age; past 90 minutes they say `last read 2h
  ago, so the numbers are not counted`, and with none, `no reading yet; conbus
  quota reads it`. `conbus auth` runs `agy models`, never `-p`. A hop to `agy` on a
  person's order and `conbus run` still start `agy` itself as an interactive
  session. `scripts/check-no-browser.sh` now fails if `doctor` or `status` leaves
  the vendor's own `~/.gemini` directory behind.

### Fixed

- A log that refuses a write, as a full disk or a read-only segment does, no
  longer destroys the record. The daemon holds it in memory, up to 16 MiB, and the
  hook spools a copy if it can. The record lands once, in arrival order, when the
  log takes writes again, its spool copy goes with it, and the outage leaves a drop
  line. A spooled entry goes with a drop line only once the file is seven days old
  and the log has refused for seven days without one write working. Only a hook's
  request is held: `conbus hop` and a lease are told the write failed, and it stays
  failed. A short write no longer leaves a torn line. A spooled Inference frame
  still answers `allow`. `conbus doctor` says on its `writable` line when the log is
  refusing and how many records are at risk, by asking the daemon. A nearly full
  disk that takes short records and refuses a long one leaves no drop line while
  nothing has waited or been lost, and `daemon.log` hears about it at most every ten
  minutes.
- A recorder asked to stop, by SIGTERM, SIGINT or the SIGHUP a closed terminal
  sends, now lands what the log will take first, for up to five seconds, and leaves
  `the recorder stopped with records it could not land` for what it could not, with
  no copy on disk. A record only in memory used to go with no trace, and a hangup
  ended the recorder at once, also after a start whose repair had to run in the
  recorder itself. It also goes when asked with a client still talking. A recorder
  started with hangups ignored, as `nohup` starts it, still ignores them.
- A log deleted or moved while the recorder runs no longer swallows what comes
  after it. The recorder kept writing to the file it held open, so each record
  reported success and nothing could ever read it, even after a restart. It now
  checks that the file is still at its name before each write, and starts again
  at that name if not.
- A hook that connects to a daemon just as it closes now spools its record instead
  of dropping it. On macOS that failure shows up as `could not bound the daemon
  call`, which is a restart like any other.
- A second `conbus daemon` no longer writes anything before it gives up. It used
  to overwrite the live daemon's stamp, fill gaps and drain the spool, and only
  then lose the socket, which could leave a duplicate `seq` on disk. A daemon now
  takes a lock on `daemon.lock` first, waits up to five seconds for the recorder
  already there to go, and otherwise exits having written nothing. A start-up repair
  that outlives its daemon holds that lock until it ends, so the next daemon waits
  for it rather than writing beside it. A repair that never returns gives up after
  two minutes, takes the Codex app-server it was waiting on with it, and the daemon
  serves without it. `backfill --apply`, `--gaps --apply` and `--chat-export
  --apply` take the same lock before writing and hold it until they are done, or
  refuse at once if a recorder or its start-up repair holds it: a fill beside a
  start used to write every turn it filled twice.
- A daemon that writes a record and dies before replying, or before deleting the
  spool file it drained, no longer leaves a second copy. A new daemon now reads
  the newest hook ids back from the log at start, so a re-delivery is answered
  rather than written again.
- `usage` no longer counts a spool re-delivery twice in `records`. A second line
  under one hook id adds one to `records` and one to `repeated`, as a twin
  delivery does.
- A restart no longer writes a spooled turn twice, through the transcript pass or
  the Codex pass. Gap fill leaves what the spool holds to the drain, so the hook's
  records are the ones written, and a turn still running at the restart does not
  get an ending from its partial text.
- A tool event that arrives before its session has a conversation now waits in the
  spool for up to a day for the prompt that binds it, up to 8 MiB, instead of being
  deleted at the next two-second tick. Whether it survived used to come down to
  timing. A turn's end or a session's end with no conversation still goes at once.
  Neither `doctor` nor the drop text puts such an orphan down to a restart any more:
  bindings come back from the log, so it is a session whose prompt hook never
  reached the recorder.
- A newer hook's new event kind now waits in the spool for a daemon that can read
  it, as `docs/log-format.md` always said. It used to be refused as unreadable and
  dropped live, and deleted from the spool. It waits up to seven days and then goes
  with a drop line, as does an entry from a newer protocol, which used to wait for
  ever.
- A prompt drained from the spool no longer records a `briefed` or `continued`
  line for a briefing nobody received, and a chat order that waited in the spool
  is no longer carried out later with nobody told. The prompt itself is recorded;
  a person who gave an order during a restart gives it again. A quota stop that
  waited in the spool likewise starts no `hop --apply` on its own, however late it
  lands. The decision stays in the log, for `conbus hop` and the next briefing.
- A record that waited on its way to the log no longer reopens a turn that ended
  after it. A finished conversation was briefed to the next session as still
  mid-turn, and a quota stop stopped reading as one, once a late tool event landed
  behind the turn's end. The fold reads `spooled_at`, which the log already has, so
  an old log needs no migration. Only a main-thread turn end counts, a record counts
  as late only if it also reached the log after that end, and a late turn end does
  not close a turn that started after it.
- Under `encrypted_tools`, a tool body that reached the log elided rather than
  sealed is now counted, and `doctor` shows the total under `drops`. One line,
  `tool bodies elided, not sealed`, says how many since the last: at the end of a
  drain pass that added to the count, otherwise at most every ten minutes, and on
  shutdown, so a restart with a full spool cannot fill the drop log. Spooled tool
  bodies are still elided, because sealing in a hook would mean reading the key
  there. A Keychain that does not answer is counted the same way.
- `install` now quotes the binary's path in the hooks it registers when the path
  needs it. With a space in the home folder, every hook failed to start, so nothing
  was recorded. `conbus doctor` reported it, and nothing else did.
- A conversation reserved for a directed hop is taken only by a prompt, as the
  hop's own comment always said. A Cursor tool call or completed turn from an
  unbound session used to take it; such a call now gives the seat up, including
  across a restart, so a stranger's later prompt cannot take it instead, and both
  sessions hold fresh ids. A daemon and a restart work out seats the same way, from
  the records in the log: a hop the log refused no longer costs anyone a seat or
  leaves one behind, a prompt that used up a contested seat stays used up after a
  restart, and a leg that has arrived ends a second hop live as well as in a replay,
  so a stranger cannot sit on that conversation. A seat now lasts a day, counted
  from the `routed` record. A hop nobody followed up used to hold it for ever, and an
  unrelated prompt days later bound to the conversation for good.
- Gap fill and `backfill` no longer copy a parent conversation into its fork. A
  transcript's session is its file name, and lines naming another session are
  counted as not read; before, a Claude Desktop continuation that changed near a
  restart could gain every turn of its parent under its own `bus_id`. A forked Codex
  thread lists its parent's turns as its own, under the parent's turn ids, so a turn
  older than its thread is counted as not read. A fork's copy of its parent's turn is
  not written under the fork even when the copied lines carry the fork's own id: a
  turn id another conversation holds, or has waiting in the spool, is not missing
  here; transcripts are read oldest file first, so a parent gets to a turn neither
  holds, unless the parent used up a budget; a line stamped well before its own file
  was created is taken for a copy, in a fill only; and one prompt id with different
  words on its prompt lines is left out whole. Records already written stay.
- Gap fill and `backfill` no longer record vendor bookkeeping as turns: lines
  marked `isMeta` without a `promptSource`, compaction summaries, interrupts,
  local-command output and the vendor's own error messages. A skill's body or a
  compaction no longer ends the turn it sits inside. A turn the person
  interrupted now has no ending, as on the live path. Records already in the log
  stay: the log is append-only.
- Gap fill dedupes on the right keys. A vendor turn id decides, so a missed turn
  whose words were said before is filled instead of skipped. Every conversation
  a split session was written under is consulted, so its older turns are not
  written again. A prompt whose hook was lost is filled even when the turn's other
  events arrived, without a second ending.
- A recovered turn ends only when the transcript shows the model stopped. Before,
  any reply counted, so a turn still running or cut off on a tool call was recorded
  as `Complete` with partial text, and its real `Stop` wrote a second ending. A
  missed turn a tool call ended gets its ending again, from the vendor's
  `toolEndsTurn`, with no message when the turn had no text. Turns that end on a
  tool call without that flag still get none.
- A log that cannot be read at start is no longer read as empty. Every
  conversation in a chat export looked new and was appended again. The fill now
  skips that start and keeps its range for the next one, as it does when the log
  refuses a write part way through.
- A fill no longer loses its range to what it could not read. A transcript that
  could not be read keeps it, and a Codex pass that could not run holds its range in
  `cache/gap-since-codex` until a start can read it, without holding back the Claude
  Code transcripts. `conbus backfill --gaps` now fills live Codex threads too, and
  says when it could not read them.
- A wedged `codex app-server`, or one Codex thread slower than the server's limit,
  now costs one start, not every start while its range is held, or two when a
  slower-growing thread uses the time ahead of it. A start that used up its limit on
  one leaves it alone for an hour. A thread is called stalled only on the time it
  actually got, and a note from a clock that ran ahead does not stand. By hand,
  `conbus backfill --gaps` always asks, and `--gaps --apply` ends the wait once it
  has read the thread.
- A budget cut can no longer starve the transcripts or threads behind it. What a
  held range has read is kept in `cache/gap-read`, so the next start goes on from
  there instead of repeating the same prefix.
- A refusal from the app-server keeps the Codex range unless the server says it has
  no such method. A temporary refusal used to lose the range. A thread the server
  refuses is asked again only when its rollout changes or by hand, so it stops costing
  a start.
- In the `thread/list` fallback a thread is read again at each start while a range is
  held, so a new turn in it is no longer skipped as already read.
- Codex turns are now asked for oldest first, and sorted here by start time as well in
  case a server ignores the request. `thread/turns/list` lists newest first unless
  asked, so a running turn could get an older turn's reply as a false ending, and a
  filled batch ended on its oldest turn. Threads already recovered in the wrong order
  stay as they are.
- A deadline now ends an app-server read or write whatever the process tree does. The
  server runs in its own process group and the whole group is killed, so a wrapper's
  child can no longer hold a start for as long as it lives. Ctrl-C, a closed terminal or
  SIGTERM takes a wedged server and its group along, in any command that started one.
- Two chat exports in one pass no longer write a conversation twice: each export now
  sees what the one before it appended. A chat export moved into the drop folder
  during a downtime is now read, whatever its file time. `cache/chat-export-read`
  says what a start has read, and reading one again no longer scans the log once
  per conversation.
- Gap fill reads only regular files. A FIFO named like a transcript or an export
  blocked a start, or `conbus backfill`, for good. Anything else is skipped and
  counted as not read, and a link is not followed.
- `cache/` is `0700` from the first time gap fill makes it.
- `log --tail N --bus-id ID` now follows aliases, as a full read does. It used to
  match the id alone, so a split conversation showed part of itself. Naming a
  conversation now costs one pass over the cache; a plain `--tail` is unchanged.
- `doctor` no longer leaves an empty `segment-00000000.jsonl` in a log folder
  with no segments. It checked that a segment could be written by creating one,
  and a later restore with `cp -n` would keep that empty file over the real one.
- A `cache/meta` sidecar now also has to match its segment's write time, so a
  segment changed without changing length is read again. Before, a damaged
  segment could read as healthy, and a restored one could keep reporting damage
  it no longer had.
- `install` and `doctor` now ask the binary `CONBUS_OPENCODE_BIN` names which
  OpenCode this is, as every other use of that binary does. `install` waits ten
  seconds to hear, not two, and says when it could not tell: a slow answer used to
  write the plugin for the wrong major with no word. `doctor` says it could not read
  the version, rather than `runs`. `cargo test` no longer runs the real `opencode`
  or `codex app-server` when they are on `PATH`.
- `conbus log | head -1` and the like no longer end in a panic. A command whose
  reader went away exits 141, as if SIGPIPE had ended it, and prints nothing. A
  hook that cannot print exits 0, because it must never fail the turn it answers.
- `conbus uninstall` now finds the log under `$XDG_STATE_HOME/conbus` when that is
  set, as every other command does. It named no log there, and left the spool it
  says it removes.
- A drop log at its 4 MiB cap silently recorded nothing more. A full `drops.jsonl`
  is now rotated to `drops.1.jsonl` and a fresh file takes new drops. One generation
  is kept, so the rotation before last is gone. Summaries read both generations, say
  `at least` once a rotation has happened, and lose only the line when a line is
  torn or holds a byte that is not UTF-8. A drop after a torn line starts on a fresh
  one, so the next reason is kept. New drop logs are `0600`. `the hook payload names
  no session` now names the payload's field names (never values), so the cause of a
  flood is visible. `scripts/canary.sh` reads both generations and compares drop
  reasons by timestamp instead of line offsets, and a drop log at its cap that it
  cannot see past no longer reads `PASS`: its `drops` item says `UNCHECKED`.
- Release packages name no account in their tar headers. `scripts/sign-release.sh
  --upload` refuses to publish a release an install would refuse: the wrong signing
  team, a checksum naming another file, a package of another version, or an
  `install.sh` that is not the script. After publishing it fetches what people get
  and says loudly if that is wrong.
- `docs/stability.md` said `spool/` and `cache/` can be deleted without losing a
  record. A spool file can be the only copy of its record, and `cache/gap-since`
  holds a range still to fill; only `cache/meta/` is safe to delete.
- The shell examples in the README and the manuals can be pasted into zsh, the shell
  a Mac opens. Interactive zsh has comments off, so a trailing `# ...` became
  arguments to the command before it, and a line that was only a comment was a
  command not found. What each line does is now said beside its block, and
  `scripts/check-docs.sh` fails on a comment in one.
- A Codex thread that goes on in a new rollout file is asked for at start again.
  Codex names that file `rollout-<stamp>-<thread>_<session>.jsonl` and compresses
  the one before it, and the fill read the session id as the thread. So the
  thread's newest file never counted for it, and turns its hooks missed were not
  filled. A by-hand fill asks every thread and was not affected.
- `scripts/canary.sh` loads rc.2 and rc.3 with their start-up gap fill off. A
  rollback to one of them, and every restart and reboot after it, starts with
  `CONBUS_GAP_BUDGET_MS=0` in the launchd plist, and their range waits in
  `cache/gap-since` for a fixed build. A real rollback stops the job before the old
  binary goes back, so no relaunch in between starts it without the switch. A
  candidate from rc.4 on has that 0 taken out before it is swapped in. `preflight`
  and `status` show the switch.

### Known limits

- Records the log refused and only memory holds are lost if the recorder stops
  before the disk has room again. Asked to stop, it says how many in a drop line if
  that line fits; killed outright, it says nothing. Until then `conbus doctor` says
  `REFUSING since … — N record(s) in memory only, lost if the recorder stops`: free
  space, and do not restart the recorder.
- With the recorder down and the disk full at the same time, a hook leaves no
  record, no spool copy and no drop line. Only gap fill can bring that turn back,
  and only if the vendor wrote it to its own transcript. A recorder started on a full
  volume cannot create its socket, so its service manager restarts it every two
  seconds until there is room.
- A `conbus hop` run while the log refuses writes fails, and has to be run again.
- Rolling back to 0.2.8 drops hooks for about the first 18 seconds after it starts
  on a large log: it takes that long to serve, and it does not spool while no daemon
  runs. Rolling back to rc.3 deletes, at its first tick and each with a drop line,
  the spool entries it cannot read: a newer hook's event kind, and tool events still
  waiting for their session's prompt.
- A hop not followed up within 24 hours gets a fresh conversation id. A Cursor
  session a hop handed a conversation to, whose prompt hook does not fire, gets its
  own id on its first tool call.
- A chat order given while the recorder restarts is recorded but not carried out, so
  give it again. A session whose first prompt waited in the spool gets no briefing.
- Tool events from a session that sends no prompt within a day are dropped, each
  with a drop line.
- A missed turn whose transcript was moved by `cp`, `rsync -a` or across volumes is
  set aside by an automatic fill, because every line in it looks older than its
  file. `conbus backfill --gaps --include-older-lines` fills it by hand and may
  write another conversation's copies with it, such as a fork's parent prompts, so
  look at a dry run first.
- A Codex thread the app-server refuses holds its fill range until its rollout
  changes, and a transcript that can never be read holds its range for as long as
  it stays. `conbus doctor` names both, and deleting `cache/gap-since` or
  `cache/gap-since-codex` gives the range up.
- What older builds wrote wrongly stays, because the log is append-only: a fork's
  copies of its parent's turns from an earlier fill or `backfill --apply`, and Codex
  turns recovered in the wrong order.
- A start-up repair that outlives its recorder keeps the log to itself until it ends,
  two minutes at most. A recorder started meanwhile waits five seconds and exits,
  and hooks wait in the spool for the next one. Do not delete `daemon.lock` to get
  past it: with a repair still running, that writes every recovered turn twice. What
  a repair that gave up had not filled is not resumed by the next start, and
  `conbus backfill --gaps` reads it. 0.2.x and earlier release candidates take no
  such lock, so until every binary on the machine is this build, one of theirs can
  write beside one of these.
- A `codex app-server` that is wedged when its recorder is killed with SIGKILL, as a
  service manager's last resort does, keeps running: SIGKILL runs no handler.
- A recorder does not start when its newest segment is empty and the one before it
  cannot be opened; hooks wait in the spool until its cap.
- Hooks registered by an earlier build in a home folder with a space in it stay
  broken until `conbus install --apply` runs again. A vendor that runs its hook
  without a shell still fails on such a path.
- `conbus backup` never holds the open segment, so a restore after losing the whole
  log loses what the backup called newer. It compares a closed segment by size, so
  a wrong file at exactly the old byte count passes a restore's last check. A backup
  removed on purpose needs a hand edit of `backup.json`.
- Packages released before this one lack `THIRD_PARTY_NOTICES.txt`, and the license
  texts of the Rust standard library and of musl, linked into the binaries, are not
  in it yet.
- The release path for a stable tag, a draft upload and then publishing it, has not
  run on GitHub before `v1.0.0`. Release candidates are prereleases and take a
  different path.

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
