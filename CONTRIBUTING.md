# Contributing

Thank you for looking. A few things make a change easy to take:

- **Read first.** `AGENTS.md` lists the constraints that are not negotiable
  (no vendor tokens, no writes into threads a person opened, no session-file
  parsing on the live path) and the decisions that are closed.
  `docs/stability.md` says what 1.x keeps; `docs/log-format.md` says what a log
  line is.
- **One change per pull request**, with a test that failed before it. The hook
  contracts are pinned by real payloads in `crates/conbus/tests/fixtures/`;
  take field names from there, not from memory or a vendor's docs.
- **Checks.** `cargo fmt --all --check`,
  `cargo clippy --workspace --all-targets --locked -- -D warnings` and
  `cargo test --workspace --locked` (with `CONBUS_SKIP_LAUNCHCTL=1` and a
  throwaway `CONBUS_HOME`), plus `sh scripts/check-public-safe.sh` and
  `sh scripts/check-docs.sh`. CI runs the same on macOS and Linux.
- **Nothing private.** No prompts, replies, paths from your machine, or
  anything from `~/.conbus` in code, fixtures, issues or commits. Fixtures are
  redacted (`/Users/USER/...`, fake ids).
- **Security problems** go to GitHub's private vulnerability reporting, not
  to an issue (`SECURITY.md`).
