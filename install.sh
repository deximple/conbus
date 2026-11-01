#!/bin/sh
# Install conbus on macOS or Linux from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
#
# Downloads the binary for this OS, checks its SHA-256, and places it at
# ~/.local/bin/conbus (written beside, then renamed, so a hook running the old
# binary is never cut off). Then it runs `conbus install`, which only reports:
# nothing in your Claude Code, Codex, Cursor, grok or opencode settings changes
# until you run `conbus install --apply` yourself.
#
#   CONBUS_VERSION=v0.1.0    a specific release instead of the latest
#   CONBUS_REPO=owner/name   a fork
#   CONBUS_BASE_URL=...      a mirror, or file:///path/to/dist for a local build
set -eu

repo=${CONBUS_REPO:-deximple/conbus}
version=${CONBUS_VERSION:-latest}

say() { printf 'conbus-install: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }

case "$(uname -s)" in
  Darwin)
    asset=conbus-macos-universal.tar.gz
    checksum() { shasum -a 256 -c "$1"; }
    ;;
  Linux)
    case "$(uname -m)" in
      x86_64)         asset=conbus-linux-x86_64.tar.gz ;;
      aarch64|arm64)  asset=conbus-linux-aarch64.tar.gz ;;
      *)              die "on Linux conbus ships for x86_64 and aarch64 only for now." ;;
    esac
    checksum() { sha256sum -c "$1"; }
    ;;
  *) die "conbus runs on macOS and Linux only for now." ;;
esac

if [ -n "${CONBUS_BASE_URL:-}" ]; then
  base=$CONBUS_BASE_URL
elif [ "$version" = latest ]; then
  base="https://github.com/$repo/releases/latest/download"
else
  base="https://github.com/$repo/releases/download/$version"
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT INT TERM

fetch() {
  if curl -fsSL "$base/$1" -o "$tmp/$1" 2>/dev/null; then
    return 0
  fi
  # A private repository answers anonymous downloads with 404. Someone with
  # access and the GitHub CLI signed in can still fetch the same assets.
  if [ -z "${CONBUS_BASE_URL:-}" ] && command -v gh >/dev/null 2>&1; then
    if [ "$version" = latest ]; then
      gh release download --repo "$repo" --pattern "$1" --dir "$tmp" --clobber >/dev/null 2>&1 && return 0
    else
      gh release download "$version" --repo "$repo" --pattern "$1" --dir "$tmp" --clobber >/dev/null 2>&1 && return 0
    fi
  fi
  return 1
}

say "downloading $asset ($version) from $base"
fetch "$asset" || die "could not download $asset"
fetch "$asset.sha256" || die "could not download $asset.sha256"
(cd "$tmp" && checksum "$asset.sha256" >/dev/null 2>&1) ||
  die "the download does not match its checksum; nothing was installed"

tar -xzf "$tmp/$asset" -C "$tmp"
"$tmp/conbus" --version >/dev/null 2>&1 || die "the downloaded binary does not run on this machine"

# Releases from the project's own repository are signed with its Developer ID
# from v0.2.6 on. A checksum only says the download is what the release holds;
# the signature says who built it. A mirror or a local build (CONBUS_BASE_URL)
# is the person's own choice and is not checked.
if [ "$(uname -s)" = Darwin ] && [ -z "${CONBUS_BASE_URL:-}" ] &&
  [ "$repo" = deximple/conbus ]; then
  case $version in
    latest | v0.2.[6-9] | v0.2.[1-9][0-9] | v0.[3-9].* | v[1-9]*)
      team=$(codesign -dv "$tmp/conbus" 2>&1 | sed -n 's/^TeamIdentifier=//p')
      [ "$team" = 4UN6WC576K ] ||
        die "the downloaded binary is not signed by conbus's Developer ID (TeamIdentifier ${team:-none}); nothing was installed"
      ;;
  esac
fi

dest="$HOME/.local/bin"
mkdir -p "$dest"
cp "$tmp/conbus" "$dest/conbus.next"
chmod 755 "$dest/conbus.next"
mv -f "$dest/conbus.next" "$dest/conbus"
say "placed $("$dest/conbus" --version) at $dest/conbus"

case ":$PATH:" in
  *":$dest:"*) ;;
  *) say "add it to PATH:  export PATH=\"$dest:\$PATH\"" ;;
esac

echo
"$dest/conbus" install
echo
say "your vendor settings have not changed. To register the hooks and start the recorder:"
say "  $dest/conbus install --apply"
say "to take everything back out later:  conbus uninstall --apply"
