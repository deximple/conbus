# Log format

The log is the source of truth. It is a directory of append-only JSONL
segments under the data root, `log/segment-00000000.jsonl` and up, rolled at
8 MiB. Everything else conbus keeps — the daemon's state, `cache/`, any index —
is derived from it and can be rebuilt from it.

## A line

One record per line, one flat JSON object, written by the recorder only. The
envelope comes first, then the event's own fields at the same level:

| Key | Type | Meaning |
| --- | --- | --- |
| `v` | integer | Schema version of the build that wrote the line (`3` today) |
| `ts` | string | UTC, fixed width (`2026-11-01T00:04:35.990Z`), so a text sort of a segment is its append order. The write time, except for recovered records, which carry the time the vendor's transcript gives |
| `seq` | integer | Sequence number, strictly increasing across segments |
| `bus_id` | string (UUIDv7) | The conversation |
| `vendor_turn_id` | string, optional | The vendor's turn correlator |
| `vendor_agent_id` | string, optional | Set when a subagent wrote the record |
| `hook_id` | string, optional | The id the hook minted, so a delivery retried through the spool is counted once |
| `spooled_at` | string, optional | When it first failed to reach the log: the hook wrote it to the spool, or the recorder kept it for a log that refused it. `ts` stays the write time |
| `harness` | string | `claude_code`, `codex`, `cursor`, `grok`, `opencode`, `agy`, `claude_chat` |
| `harness_version` | string, optional | When the vendor exposes it to its hooks |
| `vendor_session_id` | string, optional | The vendor's session id |
| `workspace` | string, optional | The checkout the vendor reported |
| `entrypoint` | string, optional | How the vendor was started (`cli`, `claude-desktop`, …) |
| `backfilled_from` | string, optional | Set on a record recovered from an official transcript or export rather than seen by a hook |
| `profile` | string, optional | The Claude Code profile (`CLAUDE_CONFIG_DIR`) the session ran under, when it was not the default; never leaves the machine |
| `event` | string | The event kind, below |

An envelope key is a reserved word for every event: no event field may reuse
one. `no_event_body_shadows_an_envelope_key` in `record.rs` holds that.

## Events

`session_candidate`, `turn_started`, `tool_requested`, `tool_batch_ended`,
`permission_asked`, `notified`, `tool_denied`, `tool_use`, `turn_ended`,
`briefed`, `routed`, `continued`, `summarised`, `leased`, `aliased`,
`session_ended`. Their fields are the variants of `Event` in
`crates/conbus-core/src/event.rs`, and each is documented there.

Text a person or a model wrote is a body: `{"kind":"kept","text":…}`,
`{"kind":"clipped","text":…,"original_bytes":…}`,
`{"kind":"elided","bytes":…}` (withheld by the retention level) or
`{"kind":"sealed","alg":…,"nonce":…,"ciphertext":…,"original_bytes":…}`
(`CONBUS_RETENTION=encrypted_tools`). Retention is applied inside the writer,
so no caller can put a withheld body on disk.

## How it changes

- **Additions only within 1.x.** A new event kind, a new optional field, or a new
  envelope key that no event uses. `v` stays `3` for additions.
- **Reading old lines.** A 1.x build reads every record any earlier build wrote.
  An older `v` is read as the current record: a field the older build did not
  write takes its default, and a field that no longer exists is ignored. The
  golden segments in `crates/conbus-core/tests/fixtures/` pin this for `v`
  1, 2 and 3.
- **Reading newer lines.** A line with a `v` newer than the build reading it is
  reported as unreadable rather than guessed at (`conbus log` and `doctor`
  count them). An event kind the reader does not know is reported the same
  way, and its `seq` is still honoured, so a newer writer's numbers are never
  reused.
- **A torn line** (a crash mid-write) is reported as unreadable, and the next
  append starts on a fresh line rather than gluing onto it.
- **Rolling back.** Because changes are additions, an older 1.x build reads a
  newer 1.x build's log except for the event kinds the older one does not
  know, which it reports as unreadable and skips. Nothing is rewritten in
  place.

## The wire

Hooks reach the recorder over a Unix socket (`conbus.sock`, mode `0600`) as one
JSON request per line. `PROTOCOL_VERSION` (`crates/conbus/src/wire.rs`) moves
whenever the request's shape does. A recorder refuses a hook that speaks a
newer protocol and says so ("Restart the daemon"); a hook that speaks an older
one is served.
