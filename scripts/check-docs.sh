#!/bin/sh
# Keep the docs honest about the build they describe:
#   - the version the README and both manuals headline is the workspace version;
#   - every command `conbus --help` lists is in the README and in both manuals.
# CI runs this on every pull request (.github/workflows/ci.yml).
set -eu
cd "$(dirname "$0")/.."

fail=0
say() { printf 'check-docs: %s\n' "$*" >&2; fail=1; }

version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
[ -n "$version" ] || { echo "check-docs: no workspace version in Cargo.toml" >&2; exit 1; }

grep -qF "**$version · " README.md || say "README.md does not headline $version"
for doc in docs/manual.md docs/manual.ko.md; do
  grep -qF "**conbus $version**" "$doc" || say "$doc does not describe conbus $version"
done

# The USAGE text in cli.rs is exactly what --help prints.
commands=$(sed -n '/^pub const USAGE/,/^";/p' crates/conbus/src/cli.rs |
  sed -n 's/^    conbus \([a-z][a-z-]*\).*/\1/p' | sort -u)
[ -n "$commands" ] || { echo "check-docs: found no commands in USAGE" >&2; exit 1; }
for command in $commands; do
  for doc in README.md docs/manual.md docs/manual.ko.md; do
    grep -qF "conbus $command" "$doc" || say "$doc never mentions \`conbus $command\`"
  done
done

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "check-docs: versions and commands agree ($version; $(echo "$commands" | wc -l | tr -d ' ') commands)"
