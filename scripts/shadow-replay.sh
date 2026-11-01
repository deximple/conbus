#!/bin/bash
# Replay a corpus clone through one conbus binary and emit a comparable
# report, for the S1 shadow check: run the released binary into one out-dir
# and the candidate into another, then diff the two.
#
#   scripts/shadow-replay.sh <conbus-bin> <corpus-home> [out-dir]
#
# corpus-home is a CONBUS_HOME tree — an APFS clone of a real root with log/
# inside, never the live one. Every read command runs twice, once plain and
# once with CONBUS_NO_CACHE=1, so sidecar-cache vs full-scan equivalence is
# evidenced in the same run. Outputs land in out-dir (default
# ./shadow-replay-out) under deterministic names:
#
#   segments.txt            byte sizes of every segment, in append order
#   excluded-newest.txt     the newest segment's name — it may have been
#                           mid-write when the clone was taken, so a diff
#                           compares closed segments only
#   record-count.txt        wc -l per segment plus the total
#   usage.json              usage --since 1970-01-01 --json (all-time)
#   progress.json           progress --since 1970-01-01 --json
#   verdict.txt             conbus verdict
#   doctor.txt              conbus doctor verbatim
#   doctor-numeric.txt      doctor's count/size lines: hardening, sealing,
#                           writable, hooks, disk and every digit-bearing
#                           line, minus build/root/socket/daemon/recorder
#                           (paths, pids, timestamps) and any line holding an
#                           absolute path
#   *.nocache.*             the same four commands under CONBUS_NO_CACHE=1
#   *.stderr                each command's stderr (defect counts print there)
#
# Pure bash plus the conbus binary; nothing here needs jq.
set -euo pipefail

say() { printf 'shadow-replay: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }

if [ $# -lt 2 ] || [ $# -gt 3 ]; then
    echo "usage: $0 <conbus-bin> <corpus-home> [out-dir]" >&2
    exit 2
fi
bin=$1
corpus=$2
out=${3:-./shadow-replay-out}

[ -x "$bin" ] || die "not an executable: $bin"
[ -d "$corpus/log" ] || die "$corpus has no log/ — not a conbus data root"

# The live root is never a corpus, the same rule bench-corpus.sh keeps.
# Resolve through symlinks before comparing.
corpus_real=$(cd "$corpus" && pwd -P)
state_home=${XDG_STATE_HOME:-"$HOME/.local/state"}
for live in "$HOME/.conbus" "$state_home/conbus"; do
    if [ -d "$live" ] && [ "$(cd "$live" && pwd -P)" = "$corpus_real" ]; then
        die "$corpus is the live data root — refusing"
    fi
done

mkdir -p "$out"

# Snapshot the segments before any command runs, so the report pins what was
# on disk rather than what a still-growing live segment became.
(cd "$corpus/log" && wc -c segment-*.jsonl 2>/dev/null) >"$out/segments.txt" || true
# wc prints a `total` line for more than one file; it is never a name.
newest=$(awk '$2 != "total" {name = $2} END {print name}' "$out/segments.txt")
printf '%s\n' "${newest:-none}" >"$out/excluded-newest.txt"
(cd "$corpus/log" && wc -l segment-*.jsonl 2>/dev/null) >"$out/record-count.txt" || true

fails=0

# replay <base> <ext> <conbus args...>: the command once plain, once under
# CONBUS_NO_CACHE=1, stdout and stderr to separate files.
replay() {
    base=$1
    ext=$2
    shift 2
    if ! env CONBUS_HOME="$corpus" CONBUS_SKIP_LAUNCHCTL=1 "$bin" "$@" \
        >"$out/$base.$ext" 2>"$out/$base.$ext.stderr"; then
        say "FAILED: $base (default) — see $out/$base.$ext.stderr"
        fails=$((fails + 1))
    fi
    if ! env CONBUS_HOME="$corpus" CONBUS_SKIP_LAUNCHCTL=1 CONBUS_NO_CACHE=1 "$bin" "$@" \
        >"$out/$base.nocache.$ext" 2>"$out/$base.nocache.$ext.stderr"; then
        say "FAILED: $base (nocache) — see $out/$base.nocache.$ext.stderr"
        fails=$((fails + 1))
    fi
}

# doctor's numeric lines: the named state lines even without a digit, plus
# every digit-bearing line, minus the ones embedding paths (a ' /' is an
# absolute path — tmp names differ between clones) or a pid/timestamp.
doctor_numeric() {
    src=$1
    dst=$2
    awk '
        /^[a-z]/ && $1 ~ /^(build|root|socket|daemon|recorder)$/ { next }
        / \// { next }
        $1 ~ /^(hardening|sealing|writable|hooks|disk)$/ { print; next }
        /[0-9]/ { print }
    ' "$src" >"$dst"
}

replay usage json usage --since 1970-01-01 --json
replay progress json progress --since 1970-01-01 --json
replay verdict txt verdict
replay doctor txt doctor
if [ -f "$out/doctor.txt" ]; then
    doctor_numeric "$out/doctor.txt" "$out/doctor-numeric.txt"
fi
if [ -f "$out/doctor.nocache.txt" ]; then
    doctor_numeric "$out/doctor.nocache.txt" "$out/doctor-numeric.nocache.txt"
fi

say "newest segment (excluded from a closed-segment diff): ${newest:-none}"
say "wrote report to $out:"
(cd "$out" && ls -1 | sed 's/^/  /')
[ "$fails" -eq 0 ] || die "$fails command(s) failed"
