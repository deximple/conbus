# Publication review

What making `deximple/conbus` public would expose, as of 2026-09-23. Making it
public publishes **every commit**, not just the current tree, so both were
read: the working tree and all 106 commits reachable from every ref.

## Verdict

**Not yet.** The current tree is clean. One item lives in history and can
only be removed by rewriting it, which is the operator's call.

## What was checked

- Secrets: API keys, tokens (GitHub, OpenAI, xAI, AWS, Google, Slack), private
  keys, JWTs, credentials in URLs. **None**, in the tree or in history. The
  only key material is the test KEK (`"aa".repeat(32)`); the real one lives in
  the macOS login Keychain and is never logged.
- `HANDOFF.local.md` (gitignored, holds live ids): **never committed**, checked
  commit by commit.
- Dangerous files (`.env*`, `*.pem`, credentials, session dumps): none tracked.
- Emails: only the commit author identity and GitHub's noreply addresses.
- Fixtures captured from real sessions: the large Codex payload is already
  redacted (`codex_0.152.1.jsonl`); paths in fixtures use placeholders
  (`/Users/USER`, `/Users/x`).

## Findings

### History only — needs a rewrite (operator)

- **`HANDOFF.md` in commits `ca4474f` (2026-09-06) through `ac42cca`
  (2026-09-08)** quoted a live hook run against a path that names the macOS
  user and one of the operator's private projects. `ac42cca` removed it; every
  ref still contains the commits that had it.

### Fixed in the current tree

- A test string named one of the operator's private projects
  (`crates/conbus-core/src/continuation.rs`); it is now a neutral name. Older
  commits still hold it — a rewrite can take it out in the same pass.
- `docs/saas-hypothesis.md` named three private codenames; it was generalized
  before its first commit, so history never held it.

### Accepted

- Test ids (`bus_id`, thread and session ids) are UUIDv7 values whose embedded
  times match the development window, so some were likely captured rather than
  invented. An opaque id grants nothing and the paths beside them are generic.
- The design plan (`.cursor/plans/…`) is dense internal Korean shorthand. It is
  confusing to a stranger, not sensitive.

## Before flipping visibility

These rewrite history. Every clone, worktree and open branch has to be
recreated afterwards, so do it once, with nothing in flight.

```bash
# 1. A fresh mirror, so the working checkouts are untouched until the end.
git clone --mirror https://github.com/deximple/conbus conbus-mirror.git
cd conbus-mirror.git

# 2. One file of replacement rules, never committed. List the exact strings
#    from `git log --all -p -S'<string>'`; `==>` gives the replacement.
printf '%s\n' '<the path quoted in ca4474f>==>/Users/USER/project' \
              '<the private project test name>==>nightly-report' > ../replacements.txt

# 3. Rewrite every ref (needs git-filter-repo: `brew install git-filter-repo`).
git filter-repo --replace-text ../replacements.txt

# 4. Confirm nothing survived, then push the rewritten history.
git log --all -p | grep -c '<the string>'   # must print 0
git push --force --mirror
```

Then delete merged remote branches, re-clone every working checkout, run this
review again, enable private vulnerability reporting (Settings → Security),
and only then:

```bash
gh repo edit deximple/conbus --visibility public --accept-visibility-change-consequences
```

The alternative is to publish a new repository whose first commit is the
current tree. It loses the pull-request history the handoff and changelog
refer to, and it is equally the operator's decision.
