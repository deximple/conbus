# RFD 0002 — What a sync may send

Status: **draft**, 2026-09-23. No sync code exists. This RFD has to be accepted
before any is written, and every field a sync sends has to be named here first.

Context: Choice 3 in [`docs/saas-hypothesis.md`](../saas-hypothesis.md) and the
plan in [`docs/saas-strategy.md`](../saas-strategy.md). The log itself never
leaves the machine; this is the contract for the one thing that might.

## Principles

1. **Counts, never content.** A sync sends how many, never what.
2. **The payload is the preview.** What `conbus usage --json --shareable` prints
   is byte for byte what a sync would send. There is no second serialiser.
3. **One-way.** The service acknowledges. It cannot send the machine a command,
   a setting, a rule or code, and the recorder opens no port for it.
4. **Opt-in per machine, revocable.** Nothing is sent until a person joins this
   machine to an organisation, and leaving stops it and forgets the credential.
5. **Nothing the contract does not name.** A new field is an amendment to this
   RFD, and a test pins the key set so that a field cannot arrive by default.

## The unit: a row

One row per UTC day, harness, attendance and — only when the organisation turns
it on — workspace. It is the `conbus_core::usage::Row` that `conbus usage`
already prints, transformed as below.

| Field | Type | Meaning |
| --- | --- | --- |
| `day` | `YYYY-MM-DD` | UTC day |
| `harness` | slug | `claude_code`, `codex`, `cursor`, `grok`, `opencode`, `agy`, `claude_chat` |
| `attendance` | `attended` \| `unattended` \| `unknown` | Whether a person was there, from the entrypoint class — never the raw entrypoint |
| `conversations` | count | Conversations with a record that day (not additive across days) |
| `turns` | count | Prompts submitted to a main thread |
| `quota_stops`, `policy_stops`, `user_stops` | count | Turns that stopped, by why |
| `subagents` | count | Subagent runs that finished |
| `tool_calls`, `tool_failures`, `tool_interrupts`, `tool_denials` | count | Tool calls that ran, failed, were stopped by a person, were refused |
| `permission_asks` | count | Times a person was asked whether a tool may run |
| `tool_ms` | count | Summed tool durations the vendor reported |
| `briefings`, `hops` | count | Briefings handed over; `routed` records |
| `workspace_key` | 16 hex chars, optional | Only when the organisation turns workspaces on; see below |
| `workspace_label` | `host/owner/repo`, optional | Only when the organisation turns names on; see below |

The envelope around a batch adds `device` (a random id minted when the machine
joins), `schema` (this RFD's version, `1`) and `sent_at`. The member is known to
the service from the device's credential; the row does not repeat it.

## Never sent

Named so that a reviewer can check the list rather than infer it:

- prompts, replies, tool inputs and outputs, error text, digests;
- reason strings (a quota or policy reason, a session-end reason, a denial
  reason), notification titles and messages;
- file-system paths, including the workspace as written;
- commands, file names, branch names, commit messages, environment variables;
- tool names (deferred, below), MCP server names;
- `bus_id`, vendor session, turn, tool-use and agent ids;
- the raw entrypoint, harness versions (deferred), hostnames, usernames;
- anything read from a vendor's auth files — conbus reads none.

## Workspaces

Off by default. A team view by repository is useful; it is also the one field
that can say what a company is working on, so the organisation chooses:

- **Off** (default): no workspace field at all.
- **Keyed**: `workspace_key` is the first 16 hex characters of
  `SHA-256(org_salt ‖ repo_id)`. `repo_id` is the normalised URL of the
  checkout's `origin` remote (`github.com/acme/api`), or `local:` plus the path
  when there is none — hashed, never sent. `org_salt` is random per
  organisation and delivered with the join credential. Two machines with the
  same repository in different folders produce the same key.
- **Named**: `workspace_label` is `host/owner/repo` from the normalised remote.
  A checkout with no remote is sent keyed, never by path.

Honest limit of *keyed*: the service knows the salt, so an operator of the
service who guesses a repository name can confirm the guess. Keying keeps paths
and names out of the service's storage and logs; it does not make the service
unable to guess.

## Views the service may build from rows

- A person's own rows, to that person.
- Team aggregates by day, harness, attendance and (if on) workspace, with any
  group of **fewer than three members hidden**.
- Per-member views only when the organisation turns named members on, and then
  visible to its admins; each member can see that the setting is on.

## Transport

- HTTPS `POST` of a JSON array of rows with the envelope. A day's row grows until
  the day ends, so a sync resends the last few days and the service upserts on
  `(device, day, harness, attendance, workspace_key|workspace_label)`.
- The device credential is conbus's own, kept in the login keychain. It is not
  and never becomes a vendor credential.
- The service replies with an acknowledgement only.

## Commands (proposed, not built)

- `conbus usage --json --shareable` — print the exact rows a sync would send,
  with the machine's settings applied. Works before joining, so anyone can see
  the payload first.
- `conbus sync --join INVITE [--apply]` — reports what would be sent and where;
  writes the credential only with `--apply`, like `install`.
- `conbus sync` — send now. `conbus sync --off` — stop, forget the credential,
  and print how to request deletion.

## Retention and deletion

Rows are kept 90 days on Team; Enterprise sets its own. A member who leaves, or
a device that is removed, has its rows deleted on request. Nothing on the
machine is deleted by leaving: the log is the person's.

## Deferred, with the question each one needs answered

- **Tool names.** Which tools fail is the most useful friction view. Built-in
  names (`Bash`, `Read`, `Edit`) say little; MCP and custom tool names can name
  internal systems. Proposal: built-ins by name, everything else counted as
  `other`. Needs a pilot to show whether `other` is too coarse.
- **Harness versions.** Useful for "this vendor release broke hooks", and
  fingerprinting at small team sizes. Needs a threshold rule.
- **Cost and tokens.** Not in the log today. If the recorder later receives a
  vendor's OpenTelemetry locally, per-row token and cost sums are counts too, and
  would be added here by amendment — never per request, never with the prompt.
- **Quota readings.** A percentage used and a reset time per account window are
  counts; which account they belong to is not. Needs a design that never keys a
  reading to a credential.

## Tests the sync code will owe

1. The shareable row serialises to exactly the key set in the table above.
2. A canary body (a prompt, an error, a path) never appears in shareable output.
3. The bytes the sync sends equal what `--shareable` printed for the same log.
4. With workspaces off, no workspace field exists — not an empty one.
