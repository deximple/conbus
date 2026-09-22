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
   `x86_64-apple-darwin`, joins them with `lipo`, and publishes
   `conbus-macos-universal.tar.gz`, its `.sha256`, and `install.sh`.

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

## Operator decisions

These are not engineering steps and no agent takes them on its own.

- **Repository visibility.** The repository is private. Until it is public,
  `install.sh`'s anonymous download returns 404 and only people with access —
  signed in with `gh auth login` — can install from a release. Making it
  public is `gh repo edit deximple/conbus --visibility public
  --accept-visibility-change-consequences`, and it publishes the whole history.
  Read `docs/publication-review.md` first.
- **Notarization.** The binary is ad-hoc signed, not notarized. `curl` does not
  quarantine what it downloads, so `install.sh` works; a copy downloaded in a
  browser needs `xattr -d com.apple.quarantine conbus`. Notarizing needs an
  Apple Developer account and its credentials in the repository secrets.
- **The name.** `conbus.sh` is the public name (`docs/www/README.md`). DNS and
  hosting for the landing page are not set up.
- **Scope.** conbus is a local tool; there is no hosted service
  (`docs/saas-hypothesis.md`). Revisit that file before adding one.
