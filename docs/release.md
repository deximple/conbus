# Releasing conbus

How a version goes from `main` to a release someone else can install, and the
steps that stay with the operator.

## Cutting a release

1. **Everything is on `main` and CI is green.** The `ci` workflow runs fmt,
   clippy with warnings as errors, and the whole suite on every pull request.
2. **Bump the version** in the workspace `Cargo.toml` (`[workspace.package]
   version`) and run `cargo build` so `Cargo.lock` follows.
3. **Write the `CHANGELOG.md` section** for that version, headed
   `## [X.Y.Z] — YYYY-MM-DD`. The release workflow publishes exactly that
   section as the release notes and fails if it is missing.
4. **Dry run** (optional, recommended): run the `release` workflow by hand from
   the Actions tab or `gh workflow run release.yml`. It tests, builds both
   architectures, packages, and keeps the package as an artifact without
   publishing anything.
5. **Tag and push**:

   ```bash
   git tag vX.Y.Z
   git push origin vX.Y.Z
   ```

   The workflow refuses a tag that does not match the workspace version, tests
   the tagged commit again, builds `aarch64-apple-darwin` and
   `x86_64-apple-darwin`, joins them with `lipo`, builds static musl binaries
   for x86_64 and aarch64 Linux (testing the musl target that ships), and
   attaches `conbus-macos-universal.tar.gz`, `conbus-linux-x86_64.tar.gz`,
   `conbus-linux-aarch64.tar.gz`, their `.sha256`s, `install.sh`, and a
   CycloneDX bill of materials (`conbus.cdx.json`), with build provenance
   attested for each package (`gh attestation verify <file> --repo
   deximple/conbus`).

   **`latest` never points at an unsigned package.** A tag with a hyphen
   (`v1.0.0-rc.1`) is published as a **prerelease**, which is never `latest`.
   Any other tag is created as a **draft**; `scripts/sign-release.sh vX.Y.Z
   --upload` replaces the package with the signed, notarized one and only
   then publishes the release as `latest` (`gh release edit --draft=false
   --latest`). A release candidate is signed the same way and stays a
   prerelease.
6. **Supply chain**: `supply-chain.yml` runs `cargo deny check` (advisories,
   licences, sources; `deny.toml`) on every release tag, by hand, and weekly —
   not on pull requests.

The asset names carry no version so `install.sh` can fetch
`releases/latest/download/<name>`. A specific version is
`CONBUS_VERSION=vX.Y.Z sh install.sh`.

## Checking a package before it ships

Everything the workflow does can be run on a Mac:

```bash
rustup target add x86_64-apple-darwin
cargo build --release --locked -p conbus --target aarch64-apple-darwin
cargo build --release --locked -p conbus --target x86_64-apple-darwin
sh scripts/package.sh dist
HOME=$(mktemp -d) CONBUS_BASE_URL="file://$PWD/dist" sh install.sh
```

The last line installs into a throwaway `HOME` from the local package, exactly
as `curl … | sh` would from a release. A package whose checksum does not match
is refused and nothing is installed.

## Signing with a Developer ID

The workflow ships an ad-hoc signed binary. `scripts/sign-release.sh`
replaces the published package with one signed with a Developer ID and
notarized by Apple. The package keeps its name and its files, so `install.sh`
does not change. It runs on the operator's Mac, after the workflow has
published the tag. No secret goes into the repository or into CI: `codesign`
uses the key in the login keychain, and `notarytool` uses a stored profile.

Once, by the operator, because each step needs their sign-in:

1. Add the Developer Program account in Xcode → Settings → Accounts. For
   its team, open Manage Certificates → + → Developer ID Application. This
   needs the paid program and the Account Holder role. A free Personal Team
   cannot issue one.
2. Make an app-specific password at appleid.apple.com, and store it under a
   profile name:

   ```bash
   xcrun notarytool store-credentials conbus-notary --apple-id APPLE_ID --team-id TEAMID
   ```

3. `security find-identity -v -p codesigning` now lists
   `Developer ID Application: NAME (TEAMID)`.

For each release, after the workflow has published `vX.Y.Z`:

```bash
CONBUS_SIGN_IDENTITY="Developer ID Application: NAME (TEAMID)" sh scripts/sign-release.sh vX.Y.Z
```

This writes `target/signed/` and uploads nothing. Run it again with
`--upload` to replace the release's package and checksum. The script:

- checks the published package against its checksum;
- signs both slices with the hardened runtime, a secure timestamp, the
  `sh.conbus` identifier and `scripts/conbus.entitlements`;
- refuses a signature that names no team;
- submits a zip to Apple and waits for `Accepted`, printing Apple's log when
  it is not;
- asks Gatekeeper to assess the result, then repackages.

A bare binary cannot carry a stapled ticket, so Gatekeeper looks the
notarization up online the first time it checks it. Apple publishes the ticket
a few minutes after accepting it. v0.2.7 was accepted at 09:02Z on 2026-09-28,
and Gatekeeper first saw it at 09:04:42Z. So the script waits up to ten
minutes for Gatekeeper before it packages anything.

`CONBUS_SIGN_IDENTITY=-` is a rehearsal. It signs ad hoc with the same
runtime and entitlements, and never contacts Apple or uploads. conbus has
been run under the hardened runtime this way — recorder, hook, `doctor`,
`usage` — with core dumps off and memory locked as before. The one
entitlement lets a landed hop open Terminal through `osascript`. That path
has not been exercised under the hardened runtime.

## Reading the canary's verdict

`scripts/canary.sh verdict` judges the window since `canary.sh start`, and its exit
code is the answer:

| Exit | Meaning |
|---|---|
| 0 | Pass. Every item was judged. `operator-feel` is still yours to answer. |
| 1 | An item failed. |
| 2 | It refused its arguments, or a number in the environment or in `canary.json`. |
| 3 | The window is still open. A fail inside the window still exits 1. |
| 4 | The window is over and an item other than `operator-feel` could not be judged. It names what is open. This is neither a fail nor a pass. |

`--idle <harness|day>[,...]` is your answer to the one question a log cannot settle:
"was this harness or day idle, or not recorded?". The question names what to pass, and
a PASS prints the answer. Nothing else can be answered this way.

`start` keeps what the verdict needs to judge the window in `canary.json`, including
the budgets it ran under: `CONBUS_CANARY_RSS_MB`, `CONBUS_CANARY_DOCTOR_MS`,
`CONBUS_CANARY_SERVING_LIMIT` and `CONBUS_CANARY_DISK_WARN_OK`. The verdict prints each
one with where it came from, and the environment still wins. A window started by an
older script has none of this recorded, and exits 4 on `drops` and `log-files`.

Where a rollback goes: `start` copies the binary installed at that moment to
`conbus.rollback`, replacing any copy left from an earlier window. A rollback, the
automatic one after a failed `start` or `canary.sh rollback`, returns to that build, the
one that ran before the swap. `usage-parity` compares against a separate copy,
`conbus.parity`: the binary named with `--parity <binary>`, or by default the build
being replaced. It is copied first, so `--parity ~/.local/bin/conbus.rollback` compares
against an older copy before `start` replaces it. `preflight` and `start` print both
builds by version and hash.

`start` exits 1 when it rolled back, or when it found no candidate or install to swap.
It exits 2 when it refused before anything moved.
`start` and `rollback` exit 3 when the rollback could not be verified, and say which
check it was. `restart` exits 1 when the daemon never served.

## Operator decisions

These are not engineering steps and no agent takes them on its own.

- **Repository visibility.** The repository is public, so `install.sh`'s
  anonymous download works for anyone. Making it public published the whole
  history; what that exposed is in `docs/publication-review.md`.
- **Notarization.** The workflow's binary is ad-hoc signed. Each release is
  then signed and notarized on the operator's Mac, as in "Signing with a
  Developer ID" above.
  - The certificate and the `conbus-notary` profile live in that Mac's login
    keychain. `HANDOFF.local.md` names them; they never go into the
    repository.
  - Apple's first review, of v0.2.6 on 2026-09-24, took about three hours. A
    re-run finds the ticket by code hash and does not submit again.
  - Until the signed package replaces the workflow's, a copy downloaded in a
    browser needs `xattr -d com.apple.quarantine conbus`.
- **The name.** `conbus.sh` is the public name (`site/README.md`). DNS and
  hosting for the landing page are not set up.
- **Scope.** conbus is a local tool; there is no hosted service
  (`docs/saas-hypothesis.md`). Revisit that file before adding one.
