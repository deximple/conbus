# Support matrix

What was checked against a real vendor, on which version, and when. A hook
that fails does so silently, so "supported" here means a real session ran
through a development build and its records were read back, not that the
adapter compiles. Fixtures in `crates/conbus/tests/fixtures/` pin what each
check saw.

## macOS (checked 2026-11-01)

| Harness | Version checked | Prompt | Tools | Turn end | Quota / stop signal | Policy deny honoured | Notes |
| --- | --- | --- | --- | --- | --- | --- | --- |
| Claude Code | 2.1.316 | yes | yes | yes | `StopFailure` (fixture 2.1.241) | yes | `effort` now on `Stop` too. Not exercised this round: `StopFailure`, permission events, `Notification`, `SubagentStop`, the status line |
| Codex | 0.166.0 | yes | yes | yes | `account/rateLimits/read` | yes | `codex exec` fires hooks (needs the official `--dangerously-bypass-hook-trust` in a throwaway run; a person's own sessions use Codex's trust dialog) |
| OpenCode | 2.2.3, 2.2.5 | yes | — | — | `session.error` (shape unverified) | n/a (plugin) | 2.x refused the 1.x plugin until this release; the 2.x plugin loads (OpenCode's own log). Turn ends, tools and tokens are published by 2.x and not yet forwarded. A persistent `opencode serve` carries `CONBUS_HOME` from its start environment — a stale server redirects plugin hooks to its old root |
| Grok | 1.0.71 | yes | (fixture 1.0.3) | yes | `StopFailure` | not run | Both registrations fire; only Grok's own copy is recorded. `StopCancelled` recorded from this release; the shutdown `Stop` ignored |
| Cursor (IDE) | unverified | | | | | n/a | The IDE is not installed on the test Mac; last verified 3.19 (fixtures) |
| cursor-agent | 2026.10.19, 2026.10.22 | **no** | yes | **no** | — | n/a | In `-p` mode only `sessionStart`, `preToolUse`, `postToolUse` and `sessionEnd` fire, so there are no turn records and no tokens; tool calls mint the conversation. `status` may claim a login while `-p` answers `AUTH_REQUIRED` — check the run, not the status |
| agy | 1.4.12 | — | — | — | `agy -p /usage` | — | Usage read only, after `agy models` shows a login |

Platform: one universal binary, signed with conbus's Developer ID and
notarized; recorder under launchd.

## Linux

| What | Status |
| --- | --- |
| Build, lint and the whole test suite on x86_64 and aarch64 | CI on every pull request (`linux`, `linux-arm`) |
| `install.sh`, the systemd user unit, fixture hooks, `doctor`, restart, `uninstall` | checked in an arm64 VM before release (see the release notes) |
| Real vendors | **unverified**: no vendor is logged in on the Linux test machine |

Static musl binaries: `conbus-linux-x86_64.tar.gz`, `conbus-linux-aarch64.tar.gz`.
