#!/bin/sh
# Guard what a public repository must not carry. Two checks over tracked
# files: a /Users/<name> path whose name is not a placeholder (USER, user, x,
# you, me, admin, someone) is a finding; and when CONBUS_PRIVATE_STRINGS
# names a readable file, every non-empty line of it that is not a comment is
# searched as a fixed string — a finding names the line number, never the
# string.
#
#   sh scripts/check-public-safe.sh                    # home paths only
#   CONBUS_PRIVATE_STRINGS=~/strings.txt sh scripts/check-public-safe.sh
set -eu

cd "$(git rev-parse --show-toplevel)"

findings=$(mktemp "${TMPDIR:-/tmp}/check-public-safe.XXXXXX")
trap 'rm -f "$findings"' EXIT

git ls-files -z | xargs -0 grep -InEo '/Users/[A-Za-z0-9._-]+/?' 2>/dev/null |
    while IFS= read -r hit; do
        file=${hit%%:*}
        rest=${hit#*:}
        line=${rest%%:*}
        name=${rest#*:}
        name=${name#/Users/}
        name=${name%/}
        case $name in
            USER | user | x | you | me | admin | someone) ;;
            *) echo "$file:$line: home path outside the allowlist" ;;
        esac
    done >>"$findings"

if [ -n "${CONBUS_PRIVATE_STRINGS:-}" ] && [ -r "$CONBUS_PRIVATE_STRINGS" ]; then
    n=0
    while IFS= read -r private || [ -n "$private" ]; do
        n=$((n + 1))
        case $private in
            '' | '#'*) continue ;;
        esac
        git ls-files -z | xargs -0 grep -IFn -e "$private" 2>/dev/null |
            while IFS= read -r hit; do
                file=${hit%%:*}
                rest=${hit#*:}
                line=${rest%%:*}
                echo "$file:$line: matches line $n of $CONBUS_PRIVATE_STRINGS"
            done >>"$findings"
    done <"$CONBUS_PRIVATE_STRINGS"
else
    echo "$0: CONBUS_PRIVATE_STRINGS unset or unreadable; checking home paths only" >&2
fi

if [ -s "$findings" ]; then
    cat "$findings"
    exit 1
fi
echo "check-public-safe: no forbidden paths or strings in tracked files"
