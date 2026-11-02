#!/bin/sh
# Keep the docs honest about the build they describe:
#   - the version the README and both manuals headline is the workspace version;
#   - every command `conbus --help` lists is in the README and in both manuals;
#   - every shell example in the README and the manuals pastes into the shell a Mac
#     opens.
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

# Interactive zsh, the shell a Mac opens, has comments off. Pasted there, a trailing
# `# ...` becomes arguments to the command before it, and a line that is only a
# comment is a command not found. So a shell block holds commands and nothing else,
# and what they do goes beside it. A `#` inside quotes, or one that does not start a
# word, is not a comment: `${x##*/}`, `$#`, a URL's `#part`. A block that starts
# with `#!` is a script's content, not something to paste, and keeps its comments.
examples="README.md docs/manual.md docs/manual.ko.md docs/beginner-manual.md docs/hop-demo.md"
# shellcheck disable=SC2086 # the names have no spaces, and each is one argument
comments=$(awk '
  function commented(line,    i, c, before) {
    before = " "
    for (i = 1; i <= length(line); i++) {
      c = substr(line, i, 1)
      if (escaped) { escaped = 0; before = c; continue }
      if (quote == sq) { if (c == sq) quote = ""; before = c; continue }
      if (c == "\\") { escaped = 1; before = c; continue }
      if (quote == dq) { if (c == dq) quote = ""; before = c; continue }
      if (c == sq || c == dq) { quote = c; before = c; continue }
      if (c == "#" && before ~ /[[:space:];&|()<>]/) return 1
      before = c
    }
    return 0
  }
  BEGIN { sq = sprintf("%c", 39); dq = "\"" }
  FNR == 1 { fence = "" }
  fence == "" && match($0, /^[[:space:]]*(```+|~~~+)/) {
    fence = substr($0, RSTART, RLENGTH); sub(/^[[:space:]]*/, "", fence)
    lang = substr($0, RSTART + RLENGTH); sub(/^[[:space:]]*/, "", lang); sub(/[[:space:]].*$/, "", lang)
    shell = (lang == "bash" || lang == "sh" || lang == "zsh" || lang == "shell" || lang == "console")
    first = 1; quote = ""; escaped = 0
    next
  }
  fence != "" {
    trimmed = $0; sub(/^[[:space:]]*/, "", trimmed)
    if (index(trimmed, fence) == 1 && trimmed ~ /^(`+|~+)[[:space:]]*$/) { fence = ""; next }
    if (first && trimmed ~ /^#!/) shell = 0
    first = 0
    if (shell && commented($0)) printf "%s:%d: %s\n", FILENAME, FNR, $0
  }
' $examples)
if [ -n "$comments" ]; then
  say "a shell example has a comment, which interactive zsh runs as words; say what it does beside the block:"
  printf '%s\n' "$comments" >&2
fi

if [ "$fail" -ne 0 ]; then
  exit 1
fi
echo "check-docs: versions, commands and shell examples agree ($version; $(echo "$commands" | wc -l | tr -d ' ') commands)"
