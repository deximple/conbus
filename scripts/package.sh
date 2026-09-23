#!/bin/sh
# Package the two release builds as one universal macOS binary.
#
#   cargo build --release --locked -p conbus --target aarch64-apple-darwin
#   cargo build --release --locked -p conbus --target x86_64-apple-darwin
#   scripts/package.sh dist
#
# Writes dist/conbus-macos-universal.tar.gz, its .sha256, and a copy of
# install.sh; the release notes for this version go to target/release-notes.md.
# Asset names carry no version on purpose: install.sh fetches
# releases/latest/download/<name>, which only works with a stable name.
set -eu

out=${1:-dist}
arm=target/aarch64-apple-darwin/release/conbus
x86=target/x86_64-apple-darwin/release/conbus
asset=conbus-macos-universal.tar.gz

for slice in "$arm" "$x86"; do
  [ -x "$slice" ] || { echo "package.sh: missing $slice — build both targets first" >&2; exit 1; }
done

rm -rf "$out"
mkdir -p "$out/stage"
lipo -create -output "$out/stage/conbus" "$arm" "$x86"
# `lipo` keeps the arm64 slice's linker signature and leaves the x86_64 one
# unsigned (0.2.4 and earlier shipped it so). Sign the whole binary ad hoc,
# and refuse to package one whose halves are not both signed.
codesign --force --sign - --identifier sh.conbus "$out/stage/conbus"
codesign --verify --strict "$out/stage/conbus"
lipo -info "$out/stage/conbus"
version=$("$out/stage/conbus" --version | awk '{print $2}')
echo "package.sh: conbus $version"

cp LICENSE README.md CHANGELOG.md "$out/stage/"
tar -czf "$out/$asset" -C "$out/stage" conbus LICENSE README.md CHANGELOG.md
(cd "$out" && shasum -a 256 "$asset" > "$asset.sha256")
cp install.sh "$out/install.sh"
rm -rf "$out/stage"

# This version's CHANGELOG section, from its heading to the next one.
mkdir -p target
awk -v v="$version" '
  /^## \[/ { if (keep) exit; keep = index($0, "[" v "]") > 0; next }
  keep
' CHANGELOG.md > target/release-notes.md
[ -s target/release-notes.md ] || { echo "package.sh: CHANGELOG.md has no section for $version" >&2; exit 1; }

ls -l "$out"
