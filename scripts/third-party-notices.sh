#!/bin/sh
# Print the license text of every crate built into conbus. The binary links them
# statically, and their licenses ask for these notices to travel with it: BSD-3-Clause
# says so outright for a binary. A crate offered under MIT or Apache-2.0 gets its MIT
# text, which is the short one and the choice its license allows.
#
# Reads what the build fetched, so it needs nothing beyond cargo. Fails when a crate
# ships no text, because then the package would go out without its notice.
set -eu
cd "$(dirname "$0")/.."

registry=${CARGO_HOME:-$HOME/.cargo}/registry/src
missing=0

printf 'conbus is built from the crates below. Each one is under its own license,\n'
printf 'whose text follows its name. conbus itself is under the LICENSE file beside this.\n'

# The targets the release builds, so a crate only another system links stays out.
crates=$(cargo tree --locked --workspace -e normal --prefix none --format '{p}|{l}' \
  --target aarch64-apple-darwin --target x86_64-apple-darwin \
  --target aarch64-unknown-linux-musl --target x86_64-unknown-linux-musl |
  sed 's/ (\*)$//' | grep -v ' (' | sort -u)

while IFS='|' read -r package license; do
  [ -n "$package" ] || continue
  name=${package% v*}
  version=${package##* v}
  source=$(find "$registry" -mindepth 2 -maxdepth 2 -type d -name "$name-$version" 2>/dev/null | head -1)
  if [ -z "$source" ]; then
    echo "third-party-notices: no source for $name $version; run cargo fetch" >&2
    missing=1
    continue
  fi
  texts=
  case $license in
    *MIT*) [ ! -f "$source/LICENSE-MIT" ] || texts="$source/LICENSE-MIT" ;;
  esac
  if [ -z "$texts" ]; then
    texts=$(find "$source" -maxdepth 1 -type f \( -iname 'LICEN[CS]E*' -o -iname 'COPYING*' -o -iname 'UNLICENSE*' \) | sort)
  fi
  if [ -z "$texts" ]; then
    echo "third-party-notices: $name $version ships no license text" >&2
    missing=1
    continue
  fi
  printf '\n================================================================\n'
  printf '%s %s — %s\n' "$name" "$version" "$license"
  printf '================================================================\n'
  printf '%s\n' "$texts" | while IFS= read -r text; do
    printf '\n'
    cat "$text"
  done
  if [ -f "$source/NOTICE" ]; then
    printf '\n'
    cat "$source/NOTICE"
  fi
done <<CRATES
$crates
CRATES

exit "$missing"
