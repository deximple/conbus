# Conbus

A conversation bus for coding agents. One conversation keeps its identity while the harness underneath it changes.

Design lives in `.cursor/plans/shared_workspace_context_3f7139b8.plan.md` and is the source of truth. Session state and the next task are in `HANDOFF.md`. Read both before proposing architecture; four review rounds already closed most of the open questions, and reopening them wastes turns. The Claude Code hook contract and the real payloads that pin it are in `crates/conbus/tests/fixtures/` — read those before changing an adapter, because field names taken from anything else have already been wrong once.

## Non-negotiable constraints

These are legal or structural, not preferences.

- Never read, store, or intermediate vendor auth tokens. Login happens only through the unmodified vendor binary's own flow.
- Never modify a vendor binary or strip its authentication.
- Never pool credentials or quota across people.
- Never parse vendor session files on the live path. Mirroring, gap filling, and routing use official channels. Backfill is the only exception, and only through a fixture-protected parser that skips unknown schemas.
- Never write into a Codex thread the user opened (`thread/inject_items`). Our warm thread is one we opened through app-server. The hook's `additionalContext` on `UserPromptSubmit` is the vendor's own context channel for Claude Code and Codex. Cursor's official inject is `additional_context` on `sessionStart`.
- Never expose a secret-reading tool to the model. Secrets are injected as child env at spawn.
- Never use vendor trademarks in product or feature names.
- Never send the rolling summary to the leg it is protecting.

## Decided, do not redesign

- Vendor TUIs stay the window. We do not build a chat UI.
- One binary, argv dispatch: `conbus daemon|hook|log|install|backfill|doctor|quota|hop|go|status|run|arena|summary|verdict|--version`. Cargo workspace internally. `hop --apply` opens or syncs a bus-owned warm on both legs (`warm:` Codex, `warm-claude:` Claude). Cursor landing is official `cursor -n <workspace>`. A chat line that *is* an order (`이제 커서로 작업할거야`) is `--to` after the hook reply. It never injects into a thread the user opened. `conbus summary` is the only model-backed digest, and it never uses the protected leg.
- JSONL segments are the source of truth. SQLite is a disposable derived index, always rebuildable from segments.
- Data in `~/.conbus/`, respecting `$XDG_STATE_HOME`.
- Derived state lives in the daemon's private memory, not a file-backed mmap ring.
- `bus_id` is minted on the first `UserPromptSubmit`, never on `SessionStart`. Cursor also mints on a completed turn when that prompt was never seen, so a live Cursor conversation can still grow a pack.
- Every log record carries `schema_version` and a vendor/version stamp.

## Working style

State assumptions before implementing, and say so when a simpler approach exists. Keep changes surgical: touch only what the task requires, and match the surrounding style. Verify with evidence rather than declaring success — for this project that usually means a real vendor session, because hook failures here are silent by default.

Self-improve is `docs/self-improve.md`. Observe the live bus, pick one live defect, fix it, verify, ledger, repeat. Do not treat a stale HANDOFF paragraph as current.

## Cursor Cloud specific instructions

CI is macOS (`macos-latest`, Rust `1.94.1` in `.github/workflows/ci.yml`). Cloud Agent VMs are Linux, and the same toolchain is what `cargo` must be. The product still runs here: `conbus daemon`, then `conbus hook --harness cursor` with a `beforeSubmitPrompt` payload on stdin, then `conbus log --tail 5`. `conbus hop --demo` needs no daemon. Set `CONBUS_HOME` when the log should not land in `~/.conbus`.

- `cargo fmt --all --check` passes on Linux.
- `cargo test --workspace --locked` passes except `install::tests::reinstalling_keeps_the_environment_an_operator_gave_the_recorder`, which shells out to macOS `plutil`.
- `cargo clippy --workspace --all-targets --locked -- -D warnings` fails on Linux: `KEYCHAIN_LIMIT` in `crates/conbus-core/src/seal.rs` and `shell_single` in `crates/conbus/src/landing.rs` are used only inside `target_os = "macos"` and clippy reports them as dead code. That gate is green on the macOS runner.
- `conbus doctor` may show `memory_locked=false` because the VM refuses `mlockall`. The daemon still records.
- CI sets `CONBUS_SKIP_LAUNCHCTL=1`. `conbus install --apply` rewrites vendor hook files and expects launchd; do not apply it into this VM unless that is the task.




