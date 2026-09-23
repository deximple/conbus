# RFD 0003 — Security design for the sync

Status: **draft**, 2026-09-24. No sync code exists. RFD 0003 and
[RFD 0002](0002-metadata-sync.md) are accepted together or not at all.
[RFD 0002](0002-metadata-sync.md) says *what* a sync may send; this RFD says how
it is protected on the way, at rest, and when it is deleted.

Why now, before any host: a pilot's security reviewer should be able to read
the design before agreeing to anything, and the first line of sync code needs
a spec. The gates in [`docs/saas-strategy.md`](../saas-strategy.md) still
decide whether a service is built at all. Until it is, pilots send the
`--shareable` file by hand.

## Scope

In scope: the path from a device's `conbus usage --shareable` rows to the
service's storage and views, and deletion from it.

Out of scope, but required before launch: admin sign-in to the web app (SSO
with MFA for Team and above), billing, and the legal texts (a DPA and a DPIA
template, RFD 0002 principle 6). The Enterprise audit export is also out of
scope; its prerequisites are listed at the end.

## What is protected

1. **Rows.** Counts per member, device and day. These are pseudonymous
   personal data.
2. **The device credential.** Whoever holds it can add rows as that device.
3. **The organisation's settings,** which decide what may be sent.
4. **The machine.** The service must never be able to reach into it.
5. **The log.** It never leaves the machine, and sync code must not make that
   less true.

## Threats

| # | Threat | Answer |
| --- | --- | --- |
| T1 | Someone on the network reads or changes a batch | TLS (D5), and a signature over the body (D1) |
| T2 | Someone replays or forges a batch | A key per device (D1); one revision per device-day, only ever higher (D4); closed days inside the retention window only (D4) |
| T3 | A device credential is stolen | The key stays in the login Keychain. It adds rows for one device only. Revocation (D3) and rotation (D2) |
| T4 | The service is breached | It holds counts, public keys and member identities. No content, no secret that can send as a device, and no IP addresses (D7). It has no path to any machine (D6) |
| T5 | The service's operator turns hostile, or is compelled | One-way (D6): no command channel. The payload is exactly what `--shareable` prints for the person. A new field needs an amendment to RFD 0002 and a client release |
| T6 | A member is re-identified from counts | Cells of at least five, complements suppressed, weekly cuts under ten members, day granularity, no IP, a random send delay (RFD 0002) |
| T7 | Sync code reads the log's content | The sync module is handed `SharedRow`s and nothing else. Key-set and canary tests (D9) |
| T8 | An update quietly adds a field | Every test run compares the envelope's key set to these RFDs. `--shareable` previews the payload with the same code |
| T9 | An invite is intercepted | Single use, bound to one member, expires in 7 days. Joining shows the organisation and its settings, and needs `--apply`. The admin sees the join and can revoke (D8) |
| T10 | The service is flooded | Size cap per batch, a daily request budget per device, invites needed to join (D7) |

## Decisions

### D1 — A key pair per device

- **Joining.** `conbus sync --join INVITE --apply` makes an Ed25519 key pair.
  - The private key goes into the login Keychain (service `sh.conbus`, account
    `sync-device-key`) and never leaves it except into memory to sign.
  - The join request sends the public key, the invite code, and a signature
    made with the new key.
  - The service answers with a `device` id: 128 random bits.
- **Every request is signed.** The body is the JSON envelope. The signature,
  in a `Conbus-Signature` header, covers the exact bytes of the body. The
  service parses the body (with a size cap first), finds the device's public
  key, and verifies the signature before it reads anything else.
- **Why not a bearer token:**
  - A token is a secret the service must hold, so a breach of the service
    would let anyone send as any device. A public key lets no one send.
  - A token that reaches a process argument, a proxy log or a crash report
    sends as the device. A signature is useless for anything but its own
    body.
- **Cost.** One new dependency tree (`ed25519-dalek`), for the sync only.

### D2 — Rotation

- **Device keys** rotate every 90 days, and only on the device's initiative.
  With its next batch, the device sends a new public key signed by the old one.
  The service accepts the new key and honours the old one for 7 more days, so
  a lost acknowledgement cannot lock the device out.
- **Invite codes** are single use, expire after 7 days, and an admin can
  withdraw one before it is used.
- **Service certificates** come from a public CA and renew automatically.

### D3 — Revocation

- **By the service.** An admin revokes a device, or a member (which revokes
  all their devices). From then on the service answers that device's
  signatures with `revoked`. The client then:
  - stops sending;
  - deletes its key from the Keychain;
  - tells the person on the next `conbus sync` or `doctor`;
  - keeps the log.
- **By the device.** `conbus sync --off` sends a signed `leave`, deletes the
  key, and prints how to delete what was sent (D7). It works even when the
  service cannot be reached: the key is deleted anyway, and the service marks
  the device silent after 30 days.

### D4 — Replay and ordering

- **The envelope** is `device`, `schema`, `day_closed`, `revision` and `rows`.
  - `revision` starts at 1 for a device-day, and rises when a later scan
    changes that day's counts, for example after `backfill`.
  - The service keeps only the highest revision it has accepted.
  - An equal revision with an identical body is answered "already have it".
  - An equal revision with a different body, or a lower revision, is
    rejected.

  So a replayed old batch cannot undo a correction.
- **Which days are accepted.** The service rejects a `day_closed` that has not
  ended in UTC by the service's clock. It also rejects one older than the
  retention window (90 days on Team), so a stolen key cannot write a year of
  history.
- **No timestamps.** The envelope carries no send time, and the service
  records none finer than the day (RFD 0002). Replay protection therefore
  rests on the device, day and revision, not on a clock.

### D5 — Transport and TLS

- **One destination.** HTTPS `POST` to the one origin written in the invite.
  The client refuses `http://`, follows no redirects, and uses only the
  system's proxy settings.
- **TLS.** Version 1.2 at least, 1.3 preferred. Certificates are checked
  against the system trust store, so a CA an organisation installs through
  MDM works. The service sends HSTS.
- **Nothing secret in a request.** An organisation's TLS-inspecting proxy can
  read the counts, which are its own data, but it cannot send as the device.
- **Proposed implementation.** Send with `/usr/bin/curl`: the body on stdin,
  and only the URL and the signature on the command line. conbus then gains
  no TLS stack, and uses the system's trust store the way it already uses
  `security` and `unzip`. A Rust TLS client that reads the system trust store
  would meet the same policy. The implementation PR chooses between them.

### D6 — One-way

- **What the client reads from a response:** the status code, and one of
  three answers:
  - `accepted` with a revision;
  - `revoked`;
  - `pause_until` with a day.

  Every other field is ignored. Each of the three stops or narrows what the
  client sends; none can widen it.
- **What the client never does with anything the service sends:** execute it,
  download it, update itself from it, or turn it into a setting. There is no
  remote configuration. Settings arrive once, in the invite (D8), and
  changing them means a new invite that the person applies.

### D7 — The service's storage

- **Region.** An organisation chooses one when it signs up: US or EU. Rows,
  backups and logs stay in that region, and subprocessors are listed in the
  DPA. A pilot runs in one region, named in its agreement.
- **Encryption at rest.** The database and backups are encrypted with AES-256
  under keys the provider manages. Per-organisation keys are an Enterprise
  item for later.
- **Stored:** rows; envelope fields; device public keys and join days;
  member identities from the invite or SSO; revisions.
- **Not stored:**
  - client IP addresses (the edge drops them from its logs, and rate limits
    are counted in memory);
  - user agents;
  - send times finer than the day.
- **Limits.** A batch is capped at 64 rows and 64 KiB. Each device gets a
  budget of 10 requests a day. A device can join only through an invite.
- **Retention.** Rows are kept 90 days on Team. Enterprise sets its own
  window.
- **Backups** stay inside the window: each is deleted when the window plus
  7 days has passed. Restoring a backup re-applies every deletion made since
  it was taken; the deletions are kept as a ledger of ids and day ranges, not
  rows.
- **Deletion.** By member or by device, through the API or the admin page. A
  device can delete its own rows with a signed request, and a member can
  delete theirs after SSO. Each deletion returns a **receipt**, a signed JSON
  document naming:
  - the organisation;
  - the member or device;
  - the day range;
  - the number of rows removed;
  - when they left primary storage;
  - the date the last backup holding them expires.
- **Operator access.** Access to production data needs MFA and is logged. An
  organisation can ask for that log for its own data.

### D8 — What an invite carries

- The organisation's id and display name, the service origin, the region, the
  member it is for, and when it expires.
- The settings. In version 1 these can only narrow what is sent (leaving out
  a harness, for example), never widen it past RFD 0002. Workspaces stay off
  in version 1 whatever an invite says.

`conbus sync --join INVITE` prints all of it and changes nothing. `--apply`
writes the key and the settings, like `install`.

### D9 — On the device

- **What sync code can read.** The sync module takes the `SharedRow`s that
  `conbus_core::usage` produces for closed days, and has no path to segment
  bodies. That separation is structural: sync code never opens the log.
- **When it runs.** Never inside a hook, which has a 500 ms deadline. It runs
  once a day from the recorder, or when the person runs `conbus sync`, with
  a 30-second limit per request. A failure waits for the next day, with
  backoff. It never holds up recording, and is written to `daemon.log`.
- **Local record.** What was sent is kept under the private root, as the day,
  revision and a digest of the body. `conbus sync` can then say what the
  service has without asking it.

### D10 — Incidents

- **Reports.** Security reports about the client go through GitHub's private
  advisories ([SECURITY.md](../../SECURITY.md)). A monitored address for the
  service exists before the first pilot's rows arrive.
- **Breach notice.** The organisation is the controller and the service a
  processor. A breach is reported to the organisation without undue delay,
  in time for it to meet its own 72-hour duty under GDPR Art. 33.

## Enterprise audit export — prerequisites, not sync

An audit export would claim that a log is complete and unaltered. That claim
needs work the log does not have today:

- **Hash-chained segments.** Each record carries the SHA-256 of the record
  before it, and a segment's first record chains to the previous segment's
  last. A record that is removed, edited or reordered then breaks the chain,
  and `doctor` can say where.
- **Signed exports.** `conbus export --signed` writes a file and a detached
  Ed25519 signature. It uses a key kept for that purpose, not the sync key.
- **A statement of what the chain cannot prove.** A person who controls the
  machine can rewrite the whole chain. Only an anchor outside the machine can
  rule that out, for example a periodic signed chain head sent in a batch.

None of this is built before an Enterprise buyer asks for it.

## Open questions

- **Signature scheme.** Ed25519 with a new dependency (proposed), or an HMAC
  under a secret the service holds, which needs no new dependency but loses
  T4's protection. This is decided at acceptance.
- **The pilot region.** US or EU, decided by the operator with the first
  pilot.
- **Member identity.** Whether it comes from invite emails in version 1, or
  only from SSO.
- **Whether same-day alerts are ever offered.** RFD 0002 sends a day once,
  after it closes. Alerts would need their own opt-in and their own threat
  row.

## Tests owed by the sync code

1. The envelope's key set is exactly `device`, `schema`, `day_closed`,
   `revision` and `rows`, and each row's key set is RFD 0002's.
2. An open day is never sent. A day is sent once per revision.
3. The service rejects:
   - an older revision;
   - a different body at the same revision;
   - an open day;
   - a day outside the window;
   - a bad signature;
   - a revoked device.
4. The client ignores unknown response fields. No response widens what is
   sent.
5. `--off` deletes the Keychain item, and a later `sync` sends nothing.
6. The private key never appears in a process argument, a log line or a file.
7. A deletion's receipt counts what was removed. A restored backup does not
   bring deleted rows back.

## What this does not change

No service is hosted before the gates: V1's three pilots, and at least
$1k/month signed. RFD 0002's contract stands as written, apart from the one
field this RFD adds to the envelope, `revision` (D4).
