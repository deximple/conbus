# RFD 0002 — What a sync may send

Status: **draft**, 2026-09-23, revised the same day after review. The client
ships in 0.2.9; no service exists. This RFD and
[RFD 0003](0003-security-design.md), the security design, have to be accepted
before a service runs, and every field a sync sends has to be named here first.

Context: Choice 3 in [`docs/saas-hypothesis.md`](../saas-hypothesis.md) and the
plan in [`docs/saas-strategy.md`](../saas-strategy.md). The log itself never
leaves the machine; this is the contract for the one thing that might.

## Principles

1. **Counts, never content.** A sync sends how many, never what.
2. **The rows are the preview.** `conbus usage --json --shareable` prints exactly
   the rows a sync would send, from the same code. The only additions on the
   wire are the envelope fields named below.
3. **One-way.** The service acknowledges. It cannot send the machine a command,
   a rule or code. Settings travel in the invite, and the service can only ever
   ask a machine to send *less* than its settings allow.
4. **Opt-in per machine, revocable.** Nothing is sent until a person joins this
   machine to an organisation; leaving stops it and forgets the credential.
5. **Nothing the contract does not name.** A new field is an amendment here
   first, and tests pin the key set so that a field cannot arrive by default.
6. **It is personal data.** A row is tied to a member and a device, so it is
   pseudonymous personal data (under GDPR, and a works-council matter in some
   countries). A data processing agreement and a DPIA template exist before the
   first pilot.

## The unit: a row

One row per UTC day, harness and attendance. It is `conbus_core::usage::SharedRow`,
which `conbus usage --shareable` prints: the local `Row` with the workspace
removed and rows that differed only by workspace merged. Their conversation
counts add up rather than overlap, because a conversation is attributed to one
workspace.

| Field | Type | Meaning |
| --- | --- | --- |
| `day` | `YYYY-MM-DD` | UTC day |
| `harness` | slug | `claude_code`, `codex`, `cursor`, `grok`, `opencode`, `agy`, `claude_chat`; any other name is sent as `other` |
| `attendance` | `attended` \| `unattended` \| `unknown` | Whether a person was there, from the entrypoint class — never the raw entrypoint |
| `conversations` | count | Conversations with a record that day (not additive across days) |
| `turns` | count | Prompts submitted to a main thread |
| `limit_stops`, `limit_episodes` | count | Turns stopped by a usage limit; runs of them (a new episode after an hour without one, per harness) |
| `account_stops` | count | Stopped by authentication, billing, or a hold |
| `vendor_errors` | count | The vendor was overloaded or failed |
| `request_faults` | count | Invalid request, output too long, no such model |
| `other_stops` | count | A stop reason this build does not recognise |
| `user_stops` | count | A person interrupted the turn |
| `subagents` | count | Subagent runs that finished |
| `tool_calls`, `tool_failures`, `tool_interrupts`, `tool_denials` | count | Tool calls that ran, failed, were stopped by a person, were refused |
| `permission_asks` | count | Times a person was asked whether a tool may run |
| `tool_ms` | count | Summed tool durations the vendor reported |
| `briefings`, `hops` | count | Briefings handed over; `routed` records |
| `repeated` | count | Second deliveries of a report already counted — a data-quality signal |
| `recovered` | count | Records counted in this row that gap fill or backfill recovered rather than a hook saw — a data-quality signal |
| `tokens_input`, `tokens_output`, `tokens_cache_read`, `tokens_cache_write` | count | Token sums a vendor's own hook reported for a turn, today Cursor's `afterAgentResponse` only; zero where a harness reports none. Amendment of 2026-10-14 |

The envelope around one device-day adds `device` (a random id minted when the
machine joins), `schema` (this RFD's version, `1`), `day_closed` (the UTC day
the batch is for) and `revision` (1, then higher for a corrected day — RFD 0003,
D4). The member is known to the service from the device's
credential; the row does not repeat it.

## Never sent

Named so that a reviewer can check the list rather than infer it:

- prompts, replies, tool inputs and outputs, error text, digests;
- reason strings (a limit or account reason, a session-end reason, a denial
  reason), notification titles and messages — only the category in the table;
- file-system paths, including the workspace as written;
- commands, file names, branch names, commit messages, environment variables;
- tool names and MCP server names (deferred, below);
- `bus_id`, vendor session, turn, tool-use and agent ids;
- the raw entrypoint, harness versions (deferred), hostnames, usernames;
- the Claude Code profile a record came from (`profile`);
- anything read from a vendor's auth files — conbus reads none.

The service does not store the client's IP address, and keeps no send time
finer than the day.

## When a day is sent

Once, after it closes. A device sends day *D* after 00:00 UTC on *D+1* plus a
random delay of up to an hour, and never a partial day. The service keeps the
one version it receives; a late correction replaces it and leaves no history.

Resending a growing "today" would let the service reconstruct when a device was
active inside the day from the differences between sends. The cost is that the
first version has daily digests, not same-day alerts; same-day alerts would be a
separate opt-in with its own entry here.

## Workspaces

Off by default, and off in version 1. A view by repository is useful; it is also
the one field that can say what a company is working on. Proposed for a later
version, the organisation choosing:

- **Keyed**: `workspace_key` is the first 16 hex characters of
  `SHA-256(org_salt ‖ repo_id)`. `repo_id` is the normalised URL of the
  checkout's `origin` remote (`github.com/acme/api`) **with any credentials
  removed first** (`https://x-access-token:…@github.com/…` becomes
  `github.com/…`), or `local:` plus the path when there is no remote — hashed,
  never sent. Two machines with the same repository in different folders
  produce the same key. Honest limit: the service knows the salt, so an operator
  of the service who guesses a repository name can confirm the guess. Keying
  keeps paths and names out of storage and logs; it does not make guessing
  impossible.
- **Named**: `workspace_label` is `host/owner/repo` from the normalised,
  credential-free remote. A checkout with no remote is sent keyed, never by path.

Either way, adding the field splits rows the merge above joins, and the upsert
key grows to include it.

## Proposed opt-in fields (not in version 1)

- **`plan`** — a plan a member declares per vendor (`claude:max`, `cursor:pro`),
  in the envelope, so a member's own view can say "N limit episodes on plan P".
- **`automation_key`** — an organisation-salted hash of a label the person sets
  for an unattended run (`CONBUS_JOB=nightly-triage`), so "which automation
  fails" does not need workspaces or names. Needs a recorder field first.
- **Coverage** — whether each member has a joined device, so a team view can say
  "N of M members reporting".

## Views the service may build from rows

- A member's own rows, to that member.
- Team aggregates by day, harness and attendance, where **every published cell
  holds at least five members**, complementary cells that would let a small
  group be recovered by subtraction are suppressed, and teams under ten members
  see weekly cuts only.
- Per-member views only when the organisation turns named members on — visible
  to its admins, and shown to each member as on.

## Transport

- One HTTPS `POST` per device-day: the envelope and that day's rows.
- The device credential is conbus's own, kept in the login keychain. It is not
  and never becomes a vendor credential.
- The service replies with an acknowledgement only.

## Security requirements (RFD 0003, before any sync code)

Designed in [RFD 0003](0003-security-design.md), draft since 2026-09-24:
authentication and rotation of device credentials; server-side revocation;
replay protection (one accepted batch per device-day); a TLS policy; data
residency; encryption at rest; backups inside the retention window; deletion by
API with a receipt, per member and per device.

The Enterprise audit export is not a sync feature and needs its own work first:
hash-chained log segments and signed exports.

## The pipe: the same rows, into a collector the person runs

`conbus usage --otlp URL` — **built**, as a spike. It sends the rows
`--shareable` prints for the window, and nothing else, as OpenTelemetry metrics
to a collector on the person's own machine — the OpenTelemetry Collector,
Grafana Alloy and the Datadog Agent all take OTLP/HTTP on `127.0.0.1:4318` —
which forwards them to the monitoring the team already runs. That collector is
not the conbus service. There is no device id, member, credential, envelope or
conbus host, and the service's rules above (cells of five, one version per day)
are not in play: the person sends their own counts to their own system, when
they run the command. What conbus puts on the wire is still bound here — the
never-sent list applies, and every field is in the table below.

- **Wire.** One `POST {URL}/v1/metrics` (the URL as given when it already ends
  in `/v1/metrics`), `Content-Type: application/json`, plain HTTP/1.1 to a
  loopback address. `https://`, any other host, and a URL with a user name or
  password are refused before a connection is made; TLS onward is the
  collector's job. A refusal, a failed connection, an answer other than 2xx or
  a partial success is an error and a non-zero exit. Nothing is retried: a
  delta the collector took without acknowledging it would count twice.
- **Encoding.** `ExportMetricsServiceRequest` in OTLP's JSON encoding: proto
  field names in lowerCamelCase, enum values as integers
  (`aggregationTemporality: 1` is DELTA), and 64-bit integers — the timestamps
  and `asInt` — as decimal strings.
- **Resource.** `service.name` = `conbus` and `service.version` = the release
  (`0.2.5`), not the commit. No host, user, device or scope attribute; a
  collector that adds its own does so by its own configuration.
- **Points.** Every count of a row is one point, with `harness` and
  `attendance` as attributes and `kind` on `conbus.stops`. Sums are monotonic
  deltas from `startTimeUnixNano`, 00:00 UTC of the row's day, to
  `timeUnixNano`, the next 00:00 — or the moment of sending, for a day that has
  not ended. The gauge carries only `timeUnixNano`, since OTLP ignores a
  gauge's start.
- **Once a day.** Deltas add up, so a day sent twice is counted twice, and
  today is still growing: the unit is a closed `--day`, sent once.

| Row field | Metric | Type | Unit | Attributes |
| --- | --- | --- | --- | --- |
| `day` | the points' `startTimeUnixNano` and `timeUnixNano` | | | |
| `harness`, `attendance` | an attribute on every point | | | |
| `conversations` | `conbus.conversations` | Gauge | `{conversation}` | |
| `turns` | `conbus.turns` | Sum | `{turn}` | |
| `limit_stops` | `conbus.stops` | Sum | `{stop}` | `kind` = `limit` |
| `account_stops` | `conbus.stops` | Sum | `{stop}` | `kind` = `account` |
| `vendor_errors` | `conbus.stops` | Sum | `{stop}` | `kind` = `vendor` |
| `request_faults` | `conbus.stops` | Sum | `{stop}` | `kind` = `request` |
| `other_stops` | `conbus.stops` | Sum | `{stop}` | `kind` = `other` |
| `user_stops` | `conbus.stops` | Sum | `{stop}` | `kind` = `person` |
| `limit_episodes` | `conbus.limit_episodes` | Sum | `{episode}` | |
| `subagents` | `conbus.subagents` | Sum | `{run}` | |
| `tool_calls` | `conbus.tool.calls` | Sum | `{call}` | |
| `tool_failures` | `conbus.tool.failures` | Sum | `{call}` | |
| `tool_interrupts` | `conbus.tool.interrupts` | Sum | `{call}` | |
| `tool_denials` | `conbus.tool.denials` | Sum | `{call}` | |
| `permission_asks` | `conbus.permission_asks` | Sum | `{ask}` | |
| `tool_ms` | `conbus.tool.duration` | Sum | `ms` | |
| `briefings` | `conbus.briefings` | Sum | `{briefing}` | |
| `hops` | `conbus.hops` | Sum | `{hop}` | |
| `repeated` | `conbus.repeated` | Sum | `{record}` | |
| `recovered` | `conbus.recovered` | Sum | `{record}` | |
| `tokens_input` | `conbus.tokens` | Sum | `{token}` | `type` = `input` |
| `tokens_output` | `conbus.tokens` | Sum | `{token}` | `type` = `output` |
| `tokens_cache_read` | `conbus.tokens` | Sum | `{token}` | `type` = `cacheRead` |
| `tokens_cache_write` | `conbus.tokens` | Sum | `{token}` | `type` = `cacheCreation` |

`type` takes the four values Claude Code's own `claude_code.token.usage`
counter uses (`input`, `output`, `cacheRead`, `cacheCreation`; `input`
excludes cache tokens), so one dashboard query can sum both. Claude Code and Codex token and
cost counters are not carried here: Claude Code exports them itself
(`CLAUDE_CODE_ENABLE_TELEMETRY=1` and an OTLP endpoint, with its prompt and
tool-detail gates left off), into the same collector.

Checked against a real collector on 2026-09-24: the OpenTelemetry Collector
0.161.0 (core distribution, darwin arm64), with its OTLP/HTTP receiver on
loopback and the file exporter.
- It answered `200` to a real closed day from the operator's Mac: 5 rows, 100
  points. Every point it kept equalled the `--shareable` row, with the metric,
  unit, type, `DELTA` temporality, monotonic sums, day-boundary timestamps,
  and attributes tabled above.
- The resource held `service.name` and `service.version` and nothing else. A
  body with no `scope` was accepted.
- A path it does not serve came back `404` and exited non-zero, and so did a
  collector that was not running.

`tests/otlp_collector.rs` repeats the check when `CONBUS_OTELCOL` names a
collector binary; it is ignored otherwise.

Grafana Alloy 1.19.2 (darwin arm64) was checked the same day. Its
`otelcol.receiver.otlp` answered `200` to the same closed day, and its
`otelcol.exporter.otlphttp` forwarded all 100 points unchanged to a local
listener: values, units, types, `DELTA`, day-boundary timestamps, attributes
and resource. A path it does not serve came back `404`. Alloy's anonymous
usage reporting was off (`--disable-reporting`) and every listener was on
loopback.

The Datadog Agent 7.83.2 was checked the same day. It ran in a throwaway
container with OTLP/HTTP on loopback, against a trial organisation on the
`ap1` site; the operator entered the API key. Every send was answered `200`,
and the Agent forwarded without error: nothing dropped, requeued or retried.
What Datadog kept is only visible in Datadog, since the Agent reports success
either way:

| Sent | Points stamped | Kept |
| --- | --- | --- |
| 9/23, 2.4 h after it ended | 2026-09-24 00:00 UTC | yes |
| 9/24, the day not yet ended | the send time | yes |
| 9/22, 26 h after it ended | 2026-09-23 00:00 UTC | no |

For 9/23, every harness's turns, conversations and tool calls, and all six
stop kinds, equalled the `--shareable` rows. So the cut-off lies between 2.4
and 26 hours, not at the hour Datadog's metrics documentation gives for
submitted points. Send each day within a couple of hours of 00:00 UTC.

Other backends keep their own rules for late points. A Prometheus-style store
takes an old sample only inside its out-of-order window, and wants cumulative
sums: the collector's `deltatocumulative` processor, or Prometheus's own. A
day's points end at 00:00 UTC, so a send soon after that is the one most
backends keep.

Sources: [OTLP 1.11.0](https://opentelemetry.io/docs/specs/otlp/), "JSON
Protobuf Encoding" and "OTLP/HTTP";
[`metrics.proto`](https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/metrics/v1/metrics.proto)
and [`metrics_service.proto`](https://github.com/open-telemetry/opentelemetry-proto/blob/main/opentelemetry/proto/collector/metrics/v1/metrics_service.proto);
the [metrics data model](https://opentelemetry.io/docs/specs/otel/metrics/data-model/)
on delta intervals; the
[OTLP exporter configuration](https://opentelemetry.io/docs/specs/otel/protocol/exporter/)
on appending `/v1/metrics`; the
[semantic conventions for metrics](https://opentelemetry.io/docs/specs/semconv/general/metrics/)
on units; Datadog's
[historical metrics](https://docs.datadoghq.com/metrics/custom_metrics/historical_metrics/);
Prometheus's [OpenTelemetry guide](https://prometheus.io/docs/guides/opentelemetry/).

## Commands (proposed, not built unless marked)

- `conbus usage --json --shareable` — **built** (#31). Prints the rows a sync
  would send, with the machine's settings applied. Works before joining, so
  anyone can read the payload first.
- `conbus usage --otlp URL` — **built**, a spike. Sends those rows to a
  collector on this machine; see "The pipe" above. Not a sync.
- `conbus sync --join INVITE [--apply]` — **built** (0.2.9). Reports what would
  be sent and where; writes the credential and the invite's settings only with
  `--apply`, like `install`.
- `conbus sync` — **built** (0.2.9). Send the closed days not yet sent.
  `conbus sync --off` — **built** (0.2.9). Stop, forget the credential, and
  print how to delete what was sent.

## Retention and deletion

Rows are kept 90 days on Team; Enterprise sets its own. Deletion by member or by
device, with a receipt. Nothing on the machine is deleted by leaving: the log is
the person's.

## Deferred, with the question each one needs answered

- **Tool names.** Which tools fail is the most useful friction view. Built-in
  names (`Bash`, `Read`, `Edit`) say little; MCP and custom tool names can name
  internal systems. Proposal: built-ins by name, everything else counted as
  `other`. Needs a pilot to show whether `other` is too coarse.
- **Harness versions.** Useful for "this vendor release broke hooks", and a
  fingerprint at small team sizes. Needs a threshold rule.
- **Cost and tokens.** Tokens from a vendor's hook were added by the amendment
  of 2026-10-14 (the table above), as day sums — never per request, never with
  the prompt. Cost in dollars is still deferred: no hook carries it, and Claude
  Code's own telemetry already does.
- **Quota readings.** A percentage used and a reset time per account window are
  counts; which account they belong to is not. Needs a design that never keys a
  reading to a credential. "Blocked time" waits for this.

## Tests

Built with `--shareable` (#31):

1. A shared row serialises to exactly the key set in the table above.
2. A canary body — a prompt, an error, a tool name, a path — never appears in a
   shared row.
3. With workspaces off, no workspace field exists — not an empty one — and rows
   that differed only by workspace are merged with their counts summed.
4. A harness name this build does not know is shared as `other`.

Owed by the sync code:

5. The rows a sync sends equal what `--shareable` printed for the same log.
6. Credentials in a remote URL never reach a key or a label.
7. A day is not sent before it closes, and is sent once.

Built with `--otlp`, the pipe:

8. Every count of a row is one point of the metric the table names, and the
   body carries no workspace, path, prompt, reply, tool name, stop reason, raw
   entrypoint or id.
9. The body uses OTLP/JSON's keys and no others, and its resource names only
   the program.
10. `https://`, a host other than this machine, and a malformed URL are refused
    before a connection is made — by the binary, before it prints anything.
11. The request is one `POST /v1/metrics` with `Content-Type: application/json`,
    and its body is the encoding of the rows `--shareable` printed.
12. A closed port, an answer other than 2xx, a partial success and a collector
    that never answers are errors that say so, and the command exits non-zero.
