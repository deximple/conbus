#!/bin/bash
# Replay a corpus clone through two conbus binaries and diff their reports,
# for the S1 shadow check: the released build's reads against the RC's.
#
#   scripts/shadow-replay.sh OLD NEW CLONE [OUT]
#
# OLD and NEW are conbus binaries; CLONE is a CONBUS_HOME tree — an APFS
# clone of a real root with log/ inside, never the live one. Each binary's
# report lands in OUT/old/ and OUT/new/ (default OUT=./shadow-replay-out);
# DIFF-SUMMARY.txt then compares each pair: usage.json, progress.json,
# verdict.txt, doctor-numeric.txt and record-count.txt — IDENTICAL, or the
# differing lines.
#
# Every read command runs twice per binary, once plain and once with
# CONBUS_NO_CACHE=1, so sidecar-cache vs full-scan equivalence is evidenced
# in the same run. The newest segment may have been mid-write when the clone
# was taken: excluded-newest.txt names it, and a diff between runs of two
# snapshots compares closed segments only.
#
#   segments.txt            byte sizes of every segment, in append order
#   excluded-newest.txt     the newest segment's name — excluded from a
#                           closed-segment comparison
#   DIFF-SUMMARY.txt        IDENTICAL or the differing lines, per pair
#   old/, new/              one report per binary:
#     record-count.txt        wc -l per segment plus the total
#     usage.json              usage --since 1970-01-01 --json (all-time)
#     progress.json           progress --since 1970-01-01 --json
#     verdict.txt             conbus verdict
#     doctor.txt              conbus doctor verbatim
#     doctor-numeric.txt      doctor's count/state lines: hardening, sealing,
#                             writable, hooks, disk and every digit-bearing
#                             line, minus build/root/socket/daemon/recorder
#                             (paths, pids, timestamps) and any line holding
#                             an absolute path
#     *.nocache.*             the same four commands under CONBUS_NO_CACHE=1
#     *.stderr                each command's stderr (defect counts print there)
#
# Pure bash plus the conbus binaries; nothing here needs jq.
set -euo pipefail

say() { printf 'shadow-replay: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }

if [ $# -lt 3 ] || [ $# -gt 4 ]; then
    echo "usage: $0 OLD NEW CLONE [OUT]" >&2
    exit 2
fi
old_bin=$1
new_bin=$2
corpus=$3
out=${4:-./shadow-replay-out}

for bin in "$old_bin" "$new_bin"; do
    [ -x "$bin" ] || die "not an executable: $bin"
done
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

mkdir -p "$out/old" "$out/new"

# Snapshot the segments before any command runs, so the report pins what was
# on disk rather than what a still-growing live segment became.
(cd "$corpus/log" && wc -c segment-*.jsonl 2>/dev/null) >"$out/segments.txt" || true
# wc prints a `total` line for more than one file; it is never a name.
newest=$(awk '$2 != "total" {name = $2} END {print name}' "$out/segments.txt")
printf '%s\n' "${newest:-none}" >"$out/excluded-newest.txt"

fails=0

# replay <dir> <base> <ext> <conbus args...>: the command once plain, once
# under CONBUS_NO_CACHE=1, stdout and stderr to separate files.
replay() {
    dir=$1
    base=$2
    ext=$3
    bin=$4
    shift 4
    if ! env CONBUS_HOME="$corpus" CONBUS_SKIP_LAUNCHCTL=1 "$bin" "$@" \
        >"$dir/$base.$ext" 2>"$dir/$base.$ext.stderr"; then
        say "FAILED: $dir/$base (default) — see $dir/$base.$ext.stderr"
        fails=$((fails + 1))
    fi
    if ! env CONBUS_HOME="$corpus" CONBUS_SKIP_LAUNCHCTL=1 CONBUS_NO_CACHE=1 "$bin" "$@" \
        >"$dir/$base.nocache.$ext" 2>"$dir/$base.nocache.$ext.stderr"; then
        say "FAILED: $dir/$base (nocache) — see $dir/$base.nocache.$ext.stderr"
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

# report <bin> <dir>: one binary's comparable report.
report() {
    bin=$1
    dir=$2
    (cd "$corpus/log" && wc -l segment-*.jsonl 2>/dev/null) >"$dir/record-count.txt" || true
    replay "$dir" usage json "$bin" usage --since 1970-01-01 --json
    replay "$dir" progress json "$bin" progress --since 1970-01-01 --json
    replay "$dir" verdict txt "$bin" verdict
    replay "$dir" doctor txt "$bin" doctor
    if [ -f "$dir/doctor.txt" ]; then
        doctor_numeric "$dir/doctor.txt" "$dir/doctor-numeric.txt"
    fi
    if [ -f "$dir/doctor.nocache.txt" ]; then
        doctor_numeric "$dir/doctor.nocache.txt" "$dir/doctor-numeric.nocache.txt"
    fi
}

report "$old_bin" "$out/old"
report "$new_bin" "$out/new"

# The S1 diff: each comparable pair is IDENTICAL or lists its differing
# lines. A pair that could not be produced is named missing rather than
# diffed.
{
    for pair in usage.json progress.json verdict.txt doctor-numeric.txt record-count.txt; do
        a=$out/old/$pair
        b=$out/new/$pair
        if [ ! -f "$a" ] || [ ! -f "$b" ]; then
            echo "$pair — MISSING (a side did not produce it)"
            continue
        fi
        if cmp -s "$a" "$b"; then
            echo "$pair — IDENTICAL"
        else
            echo "$pair — DIFFERS"
            diff "$a" "$b" | sed 's/^/    /' || true
        fi
    done
} >"$out/DIFF-SUMMARY.txt"

say "newest segment (excluded from a closed-segment diff): ${newest:-none}"
say "wrote report to $out; the diff:"
sed 's/^/  /' "$out/DIFF-SUMMARY.txt"
[ "$fails" -eq 0 ] || die "$fails command(s) failed"
