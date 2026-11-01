# Stability

conbus 1.0.0 ends the clause the changelog carried since 0.1: "until 1.0 a
minor version may change the command line or the log format". This page is
what replaces it. Everything in the stable table keeps working, unchanged, in
every 1.x release. Anything that would break it waits for 2.0.

## Stable in 1.x

| Surface | The promise |
| --- | --- |
| Commands and flags | `install`, `uninstall`, `doctor`, `status`, `quota`, `log` (with `log index`), `usage`, `progress`, `backfill`, `run`, `go`, `hop` run by hand, `daemon`, `hook`, `--version`, and the flags each prints in its help |
| Exit codes | `0` success, `1` the command ran and failed, `2` the command line was wrong. `hook` is the exception by design: it always exits `0` and prints exactly one JSON object, because a failing hook fails the person's turn |
| Machine output | `usage --json`, `usage --json --shareable`, `progress --json`, `log --json`. Keys are only ever added. A consumer must ignore keys it does not know |
| OTLP | The metric names, units and attributes in the table in [RFD 0002](rfd/0002-metadata-sync.md) |
| The log | A 1.x build reads every record any earlier build wrote, and changes to the format are additions only ([log format](log-format.md)) |
| The data root | `~/.conbus` (or `$CONBUS_HOME`, or `$XDG_STATE_HOME/conbus`): its layout, its modes (directories `0700`, files `0600`), and the `install --apply` / `uninstall --apply` round trip, which keeps the log |
| The hook contract | [RFD 0001](rfd/0001-hook-contract.md) and the payloads pinned per harness in `crates/conbus/tests/fixtures/` |
| Platforms | macOS (one universal binary, signed with a Developer ID and notarized), Linux on x86_64 and aarch64 |

## Experimental

These work and are tested, and they may change or go within 1.x. `--help`
marks each one `(experimental)`.

- the automatic hop (a hop typed by hand is stable)
- `sync`
- `policy`
- `--profile`
- `arena`
- `summary`
- `verdict`
- `hook --inference-listen`

## What counts as breaking

- Adding a command, a flag, a JSON key, a log field, an event kind or a metric
  attribute is not breaking.
- Removing or renaming anything in the stable table, changing what a key or a
  field means, or changing an exit code is breaking, and happens in 2.0 only.
- An experimental surface that graduates moves into the stable table in the
  release that starts promising it.
- `cache/` and `spool/` under the data root are not part of the promise, and
  their format can change in any release. They are not equally safe to delete:
  - `cache/meta/` is derived from the log. Deleting it loses nothing; the next
    read rebuilds it.
  - `cache/gap-since`, `cache/gap-since-codex` and `cache/gap-read` are not
    derived. They hold the gap-fill ranges a start could not finish, for
    transcripts and for Codex threads, and how far each got. Deleting them means
    those ranges are not resumed at the next start; `conbus backfill --gaps` still
    fills them by hand. No record already written is lost.
  - `cache/chat-export-read` says which chat exports a start has read. Deleting
    it means each is read once more, which writes nothing.
  - `spool/` holds hooks in transit, and a file there can be the only copy of
    its record. Deleting one loses that record.
