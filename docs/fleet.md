# conbus on managed Macs

For an administrator deploying conbus to developer Macs that the organisation
manages. On such a Mac, Claude Code's managed settings can set
`allowManagedHooksOnly: true`, which blocks the user-level hooks
`conbus install --apply` registers — so conbus records nothing there unless its
hooks are in the organisation's own policy. This guide covers the four pieces:
the binary, the managed hooks, the per-user recorder, and approving that
recorder as a background item. Then how to take it all back out.

conbus writes no system file. `conbus install --managed` **prints** what goes
into managed settings; your device tooling puts it there.

Every path, key and rule below was checked against the vendors' documentation
on 2026-09-24 ([sources](#sources)). What has not yet been tried on a managed
Mac says so.

## 1. The binary

Install the release package on every Mac, the same one `install.sh` fetches:
`conbus-macos-universal.tar.gz` and `conbus-macos-universal.tar.gz.sha256` from
the release.

```bash
# as root, from your device tooling's install script
shasum -a 256 -c conbus-macos-universal.tar.gz.sha256   # stop if this fails
tar -xzf conbus-macos-universal.tar.gz conbus
mkdir -p /usr/local/bin
cp conbus /usr/local/bin/conbus.next && chmod 755 /usr/local/bin/conbus.next
mv -f /usr/local/bin/conbus.next /usr/local/bin/conbus
```

- **A path every account can run, with no spaces.** One managed file serves
  every account on the Mac, and the hook command carries the path unquoted.
  `/usr/local/bin/conbus` is used below. (`install.sh` places the binary in the
  running person's `~/.local/bin`, which is right for one person and wrong for
  a fleet.)
- **Owned by root, in a directory only root can write** (`ls -ld` both). The
  managed hooks run this file in every account's agent sessions, so whoever
  can replace it runs code in all of them.
- **Beside, then rename — never over the running file.** Overwriting a signed
  binary in place while a process has it mapped makes macOS kill every later
  start of that path; conbus lost two hours of hooks that way (`HANDOFF.md`).
- From v0.2.6 the binary is signed with a Developer ID and notarized.
  `codesign -dv` says `TeamIdentifier=4UN6WC576K`, and `spctl --assess --type
  open --context context:primary-signature` says `Notarized Developer ID`.
  Earlier releases are ad-hoc signed (`TeamIdentifier=not set`). A copy of one
  of those downloaded in a browser needs `xattr -d com.apple.quarantine`.

## 2. Claude Code: the managed hooks

```bash
/usr/local/bin/conbus install --managed > conbus.json
plutil -convert json -o /dev/null conbus.json && echo parses
```

stdout is one JSON document and nothing else: the `hooks` block, with the same
13 events and the same command `install --apply` registers per user, naming
the binary you ran. The guidance — where it goes, how it merges, and the Codex
and Cursor pointers below — goes to stderr, so never redirect `2>&1` into the
file: Claude Code refuses to start on a Mac whose managed settings do not parse.
Run it from the fleet path; it warns when the binary is inside a home
directory or has characters a shell would split.

**Where it goes.** Claude Code applies one managed source, the highest-ranked
one that delivers a policy key — server-managed settings, then an MDM profile,
then the files — unless your policy sets `managedSourcesBehavior: "merge"`. Put
the block in the source your Macs already get; `/status` in Claude Code names
it on the `Setting sources` line.

| Source | How |
| --- | --- |
| Drop-in file | `/Library/Application Support/ClaudeCode/managed-settings.d/conbus.json`. Read after `managed-settings.json`, with the other `*.json` there in name order; lists combine, duplicates removed |
| Your file | Add the entries to the `hooks` arrays in `/Library/Application Support/ClaudeCode/managed-settings.json` |
| MDM profile | The `com.anthropic.claudecode` preferences domain: the same keys, as plist dictionaries and arrays. `plutil -convert xml1 -o - conbus.json` prints them |
| Server-managed (claude.ai console) | The same JSON. Each developer approves hooks once in a security dialog, and the policy also reaches cloud sessions, where no conbus binary exists — so prefer a file or a profile |

**How it merges.** Hook entries merge across settings levels rather than
replacing each other, so the organisation's own hooks keep running beside
these. With `allowManagedHooksOnly: true`, only managed hooks run (with Agent
SDK hooks and plugins your policy force-enables): the per-user copy is blocked
and this block is the one that records. **Without that lock, do not deploy the
block.** `install --apply` already registers the same events per user, and
Claude Code runs a handler once only when the same one is defined in more than
one file — the two copies name different binaries, so both would run and every
event would be recorded twice.

**What is left out, and why.**

- `allowManagedHooksOnly` itself. The lock is your policy, and in a drop-in a
  later file's value replaces an earlier one's, so ours would overrule yours.
- The StatusLine wrapper `install --apply` adds per user. A managed
  `statusLine` is one value that replaces every developer's; under
  `allowManagedHooksOnly` Claude Code reads it from managed settings only, and
  conbus's wrapper works by running the status command each person saved in
  their `~/.claude` — a command from a user-writable file, run under the
  organisation's authority, which is what that narrowing stops. What the
  wrapper reads, `rate_limits`, reaches a status line only for claude.ai Pro
  and Max plans (or behind a gateway with spend limits). The cost:
  `conbus quota` shows no Claude reading on these Macs.

After an upgrade, run `install --managed` again and compare: the block changes
only when the events conbus registers do.

## 3. Codex and Cursor

**Codex** has managed hooks. `install --managed` prints a pointer with the
events and command; in outline:

```toml
# /etc/codex/requirements.toml, or base64 in the com.openai.codex profile key
# requirements_toml_base64
allow_managed_hooks_only = true   # so the per-user copy does not run as well

[hooks]
managed_dir = "/usr/local/bin"    # must be absolute and exist

[[hooks.SessionStart]]
[[hooks.SessionStart.hooks]]
type = "command"
command = "/usr/local/bin/conbus hook --harness codex"
# … the same two tables for each event install --managed lists; never async
```

Codex marks hooks from `requirements.toml`, MDM or cloud sources as managed and
trusted by policy. The copy `install --apply` writes to `~/.codex/hooks.json`
is not: each developer trusts it once in Codex (`/hooks`). conbus writes
neither the requirements nor any trust record. Not yet tried against a Codex
install with managed requirements; `doctor`'s `codex` line reads only the
per-user trust records.

**Cursor** has managed hooks too — `/Library/Application Support/Cursor/hooks.json`
placed by MDM, or team hooks from its dashboard on Enterprise plans — but runs
hooks from every source, and its hooks docs name no way to block a person's
own. So the per-user `install --apply`, which registers `~/.cursor/hooks.json`,
works on a managed Mac; a managed copy as well would record every event twice.
Use one.

**grok and opencode** have no managed path in conbus: the per-user
`install --apply` registers them.

## 4. The recorder, per user

The recorder and its log are per person, so each person runs, once, in their
own login session:

```bash
conbus install --apply
```

That places `~/.local/bin/conbus`, writes
`~/Library/LaunchAgents/sh.conbus.daemon.plist` and loads it into their
`gui/<uid>` domain, and registers the per-user hooks for every vendor present —
dormant for Claude Code under `allowManagedHooksOnly`, and for Codex under
`allow_managed_hooks_only`. Until a person has run it, the managed hooks find
no recorder: they answer at once, record nothing, and note the drop in
`~/.conbus/drops.jsonl`.

After replacing the fleet binary, each person runs `conbus install --apply`
again. It reloads the recorder on the new copy; until then, a hook from a build
whose wire protocol is newer than the running recorder's is refused and its
events are dropped.

## 5. Approving the background item

macOS Background Task Management records the agent in
`~/Library/LaunchAgents/sh.conbus.daemon.plist` and shows it to the person
under System Settings > General > Login Items. A **Managed Login Items**
payload (`com.apple.servicemanagement`; device channel, user-approved MDM)
auto-enables and auto-allows items that match its rules. Rule types:
`BundleIdentifier`, `BundleIdentifierPrefix`, `Label`, `LabelPrefix`,
`TeamIdentifier`; a rule can also carry an optional `TeamIdentifier`
constraint.

- **A `Label` rule** matches the launchd plist's `Label` exactly,
  `sh.conbus.daemon`. On its own it approves any agent with that label,
  whoever signed it. From v0.2.6 it can carry the `TeamIdentifier` constraint
  `4UN6WC576K`, so that only conbus's own binary is approved. A
  `TeamIdentifier` rule for that team works as well. Before v0.2.6 the
  binary was ad-hoc signed, with no team identifier, so only the bare
  `Label` rule could match it.

  ```xml
  <key>Rules</key>
  <array>
    <dict>
      <key>RuleType</key><string>Label</string>
      <key>RuleValue</key><string>sh.conbus.daemon</string>
      <key>TeamIdentifier</key><string>4UN6WC576K</string>
      <key>Comment</key><string>conbus recorder, one per user; v0.2.6 or later</string>
    </dict>
  </array>
  ```

- **The one refused start after each upgrade** that `HANDOFF.md` describes
  ("The first start after an upgrade is refused once") came from ad-hoc
  signatures, which are a new identity with every build. The first
  Developer ID build started on the first try when it was deployed
  (2026-09-24, `runs = 1`). But `install --apply` also reloads the agent,
  which avoided the refusal on its own, so one deploy does not show that the
  signature alone ends it.

`sfltool dumpbtm` prints the state of login and background items, including
loaded service management payloads. conbus neither generates nor installs a
profile.

## 6. Check one Mac

1. In Claude Code, `/status`: `Setting sources` names the managed source that
   carries the block.
2. Send one prompt, then `conbus log --tail 5`: the turn is there.
3. `conbus doctor` reads the per-user files only. On a Mac whose hooks come
   from managed settings, its `hooks` line describes `~/.claude/settings.json`,
   which `allowManagedHooksOnly` ignores — trust steps 1 and 2 for whether
   events arrive.

## 7. Taking it back out

In this order, so no hook is left pointing at a missing binary (Claude Code
would show a non-blocking error on every event):

1. Remove the managed entries: the drop-in or your `hooks` additions or the
   profile keys; the Codex `[hooks]` entries; any Cursor enterprise entries.
2. Each person: `conbus uninstall --apply`. It takes out every per-user
   registration, the StatusLine wrapper, the grok hook file, the opencode
   plugin, the launchd agent (stopping the recorder) and `~/.local/bin/conbus`,
   and keeps the log in `~/.conbus`, which is theirs to delete. It does not
   touch managed settings or `/usr/local/bin/conbus`.
3. Remove `/usr/local/bin/conbus` and the Login Items rule.

## Sources

Read on 2026-09-24.

- Claude Code: [Deploy managed settings](https://code.claude.com/docs/en/managed-settings)
  — [where each mechanism stores the policy](https://code.claude.com/docs/en/managed-settings#where-each-mechanism-stores-the-policy),
  [drop-ins](https://code.claude.com/docs/en/managed-settings#split-a-file-based-policy-across-teams),
  [how managed sources combine](https://code.claude.com/docs/en/managed-settings#how-claude-code-combines-managed-sources);
  [`allowManagedHooksOnly`](https://code.claude.com/docs/en/settings-reference#allowmanagedhooksonly);
  [hook locations and merging](https://code.claude.com/docs/en/hooks#hook-locations);
  [one run per handler](https://code.claude.com/docs/en/hooks#hook-handler-fields);
  [settings precedence](https://code.claude.com/docs/en/settings#settings-precedence);
  [status line `rate_limits`](https://code.claude.com/docs/en/statusline);
  [server-managed approval dialogs](https://code.claude.com/docs/en/server-managed-settings#security-approval-dialogs)
- Codex: [managed hooks](https://developers.openai.com/codex/hooks#managed-hooks-from-requirementstoml);
  [requirements locations and MDM](https://developers.openai.com/codex/enterprise/managed-configuration#locations-and-precedence);
  [`hooks.managed_dir`](https://developers.openai.com/codex/config-reference)
- Cursor: [hooks configuration](https://cursor.com/docs/hooks#configuration);
  [MDM distribution](https://cursor.com/docs/hooks#mdm-distribution);
  [cloud distribution](https://cursor.com/docs/hooks#cloud-distribution-enterprise-only)
- Apple: [ServiceManagementManagedLoginItems](https://developer.apple.com/documentation/devicemanagement/servicemanagementmanagedloginitems)
  and [its rules](https://developer.apple.com/documentation/devicemanagement/servicemanagementmanagedloginitems/rule);
  [Manage login items and background tasks on Mac](https://support.apple.com/guide/deployment/manage-login-items-background-tasks-mac-depdca572563/web)
