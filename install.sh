#!/bin/sh
# Install conbus on macOS from a GitHub release.
#
#   curl -fsSL https://raw.githubusercontent.com/deximple/conbus/main/install.sh | sh
#
# Downloads the universal binary, checks its SHA-256, and places it at
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
asset=conbus-macos-universal.tar.gz

say() { printf 'conbus-install: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }

[ "$(uname -s)" = Darwin ] || die "conbus runs on macOS only for now."

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
fetch "$asset" || die "could not download $asset. If the repository is private, run 'gh auth login' first."
fetch "$asset.sha256" || die "could not download $asset.sha256"
(cd "$tmp" && shasum -a 256 -c "$asset.sha256" >/dev/null 2>&1) ||
  die "the download does not match its checksum; nothing was installed"

tar -xzf "$tmp/$asset" -C "$tmp"
"$tmp/conbus" --version >/dev/null 2>&1 || die "the downloaded binary does not run on this Mac"

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
