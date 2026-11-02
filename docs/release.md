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
   CycloneDX bill of materials (`conbus.cdx.json`). The `release` job attaches
   the macOS package, `install.sh` and the bill of materials; each
   `release-linux` job attaches its own package and checksum.

   Build provenance is attested for each package the workflow builds, so `gh
   attestation verify <file> --repo deximple/conbus` passes on a Linux tarball.
   On the macOS package a release carries it fails with a 404 and exit 1 (checked
   on v1.0.0-rc.3): `sign-release.sh` replaced that package with one the workflow
   did not build, and no attestation covers it. What vouches for that one is its
   Developer ID signature and notarization, which `install.sh` checks by team.

   **`latest` never points at an unsigned package.** A tag with a hyphen
   (`v1.0.0-rc.1`) is published as a **prerelease**, which is never `latest`.
   Any other tag is created as a **draft**; `scripts/sign-release.sh vX.Y.Z
   --upload` replaces the package with the signed, notarized one, checks that
   everything an install fetches is on the release, and only then publishes it as
   `latest` (`gh release edit --draft=false --latest`). A release candidate is
   signed the same way and stays a prerelease.
6. **Supply chain**: `supply-chain.yml` runs `cargo deny check` (advisories,
   licences, sources; `deny.toml`) on every release tag, by hand, and weekly —
   not on pull requests.

The asset names carry no version so `install.sh` can fetch
`releases/latest/download/<name>`. A specific version is
`CONBUS_VERSION=vX.Y.Z sh install.sh`. A package records `root` as its owner rather
than the account that built it, on macOS and on Linux.

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

For each release, once all three jobs of the workflow have finished for `vX.Y.Z`:

```bash
CONBUS_SIGN_IDENTITY="Developer ID Application: NAME (TEAMID)" sh scripts/sign-release.sh vX.Y.Z
```

This writes `target/signed/` and uploads nothing. Run it again with
`--upload` to replace the release's package and checksum, and for a stable tag to
publish it. The script:

- with `--upload`, before anything else, refuses a stable release that is no
  longer a draft, and a release an install would fail on. It fetches the macOS
  package, both Linux tarballs, the checksum of each, and `install.sh`, and requires
  that each `.sha256` names its own file and matches it, that each tarball holds a
  binary of this version (read from the `conbus-version X.Y.Z;` stamp in the binary,
  since a Mac cannot run a Linux one), and that `install.sh` is really the script;
- checks the published package against its checksum;
- signs both slices with the hardened runtime, a secure timestamp, the
  `sh.conbus` identifier and `scripts/conbus.entitlements`;
- refuses a signature that names no team, or a team other than the one `install.sh`
  requires, which it reads from `install.sh`;
- submits a zip to Apple and waits for `Accepted`, printing Apple's log when
  it is not;
- asks Gatekeeper to assess the result, then repackages;
- with `--upload`, uploads the signed package and checksum, makes those same checks
  again, compares the macOS package with the signed one, and only then publishes;
- after publishing, asks `gh` which release is the latest and fetches every asset
  from its anonymous `releases/latest/download/` URL, comparing the bytes. If that
  stays wrong for two minutes it fails loudly and says how to take the release back.

A refusal leaves the release a draft (a release candidate stays a prerelease) and
says what is missing and what to do: `gh run list --workflow release.yml --branch
vX.Y.Z` to see the jobs, `gh run rerun RUN_ID --failed` for one that failed, and
then `--upload` again. Rerun failed jobs only: `gh` skips its check for an existing
release when it creates a draft, so running the whole workflow again could make a
second one. A failed upload says so on that run, because `gh` deletes an asset before
it replaces it: `target/signed/` holds the signed package to upload by hand, and on a
release candidate the public prerelease is missing it until then. A re-run finds
Apple's ticket by code hash and does not wait on Apple twice.

`scripts/sign-release-rehearsal.sh` runs all of this on a fake toolchain, and CI
runs it on every pull request. **`gh release upload` from a job onto a draft with
`GITHUB_TOKEN`, and `gh release edit` publishing a draft, have never run for real:
their first run is the first stable tag, in public, and a scratch repository could
rehearse them first, which is the operator's call.**

### Taking a bad release back

If `latest` is wrong, whether the script said so or a person found it:

1. `gh release edit vX.Y.Z --draft=true` makes it a draft again. People stop getting
   it, and the assets stay on it.
2. `gh release view` names the release people get now. If that is not the one before
   it, `gh release edit vPREVIOUS --latest`.
3. Fix what was wrong. For an asset, rerun the job that attached it, or upload it by
   hand with `--clobber`, and run `sign-release.sh vX.Y.Z --upload` again. It works on
   the draft and checks everything once more.
4. A wrong binary needs a new version: bump it, write its changelog section and cut a
   new tag. Do not move a tag people may already have fetched.

Anyone who installed in between has that binary until they install again, so say so
where they will see it.

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
