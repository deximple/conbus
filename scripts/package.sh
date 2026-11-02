#!/bin/sh
# Package the release build for this OS.
#
#   macOS:
#     cargo build --release --locked -p conbus --target aarch64-apple-darwin
#     cargo build --release --locked -p conbus --target x86_64-apple-darwin
#     scripts/package.sh dist          → conbus-macos-universal.tar.gz
#   Linux (any rust target triple; the asset is named for its arch):
#     cargo build --release --locked -p conbus --target x86_64-unknown-linux-musl
#     scripts/package.sh dist x86_64-unknown-linux-musl   → conbus-linux-x86_64.tar.gz
#
# Writes the tarball, its .sha256, and a copy of install.sh; the release notes
# for this version go to target/release-notes.md.
# Asset names carry no version on purpose: install.sh fetches
# releases/latest/download/<name>, which only works with a stable name.
set -eu

out=${1:-dist}

rm -rf "$out"
mkdir -p "$out/stage"

case "$(uname -s)" in
  Darwin)
    arm=target/aarch64-apple-darwin/release/conbus
    x86=target/x86_64-apple-darwin/release/conbus
    asset=conbus-macos-universal.tar.gz
    for slice in "$arm" "$x86"; do
      [ -x "$slice" ] || { echo "package.sh: missing $slice — build both targets first" >&2; exit 1; }
    done
    lipo -create -output "$out/stage/conbus" "$arm" "$x86"
    # `lipo` keeps the arm64 slice's linker signature and leaves the x86_64 one
    # unsigned (0.2.4 and earlier shipped it so). Sign the whole binary ad hoc,
    # and refuse to package one whose halves are not both signed.
    codesign --force --sign - --identifier sh.conbus "$out/stage/conbus"
    codesign --verify --strict "$out/stage/conbus"
    lipo -info "$out/stage/conbus"
    ;;
  Linux)
    triple=${2:-x86_64-unknown-linux-gnu}
    bin=target/$triple/release/conbus
    asset=conbus-linux-${triple%%-*}.tar.gz
    [ -x "$bin" ] || { echo "package.sh: missing $bin — build the target first" >&2; exit 1; }
    cp "$bin" "$out/stage/conbus"
    ;;
  *)
    echo "package.sh: unsupported OS $(uname -s)" >&2
    exit 1
    ;;
esac

version=$("$out/stage/conbus" --version | awk '{print $2}')
echo "package.sh: conbus $version"

# scripts/sign-release.sh reads this to check a binary it cannot run, so a build
# without it should stop here, not at the release.
LC_ALL=C grep -qaF "conbus-version $version;" "$out/stage/conbus" || {
  echo "package.sh: the binary does not carry conbus-version $version;" >&2
  exit 1
}

cp LICENSE README.md CHANGELOG.md "$out/stage/"
sh "$(dirname "$0")/third-party-notices.sh" >"$out/stage/THIRD_PARTY_NOTICES.txt"
# The owner fields go out to everyone who downloads this. The default records the
# account that built it. This spelling means the same to BSD tar and to GNU tar.
tar --owner=root:0 --group=root:0 -czf "$out/$asset" -C "$out/stage" conbus LICENSE THIRD_PARTY_NOTICES.txt README.md CHANGELOG.md
case "$(uname -s)" in
  Darwin) (cd "$out" && shasum -a 256 "$asset" > "$asset.sha256") ;;
  *)      (cd "$out" && sha256sum "$asset" > "$asset.sha256") ;;
esac
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
