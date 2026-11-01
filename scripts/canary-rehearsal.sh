#!/bin/bash
# Rehearse canary.sh end-to-end under a throwaway HOME: a fake install dir,
# fake conbus binaries, CONBUS_CANARY_REHEARSAL=1 + CONBUS_SKIP_LAUNCHCTL=1
# so the file swap runs but launchctl and `install --apply` are skipped.
# Nothing here can touch the real ~/.local/bin or ~/.conbus — HOME is a
# mktemp dir for every invocation.
#
#   scripts/canary-rehearsal.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
canary=$here/canary.sh

tmp=$(mktemp -d "${TMPDIR:-/tmp}/canary-rehearsal.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

home=$tmp/home
install_dir=$home/.local/bin
rollback_bin=$install_dir/conbus.rollback
mkdir -p "$install_dir" "$tmp/stage" "$home/.conbus" \
    "$home/Library/LaunchAgents" "$tmp/evidence"

# A fake conbus: a version, a doctor with a not-answering socket (the absent
# daemon is the expected rehearsal state) and an unreadable count, a `log
# index` that succeeds, a usage --json for the verdict, and an `install`
# that writes a marker — a rehearsal must never invoke it.
write_fake() {
    tag=$1
    path=$2
    cat >"$path" <<EOF
#!/bin/sh
case "\$1" in
    --version) echo "conbus 0.0.0-$tag" ;;
    doctor)
        echo "socket    \$HOME/.conbus/conbus.sock — not answering (rehearsal runs no daemon)"
        echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"
        echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"
        ;;
    log) echo "cache     0 closed segment(s): 0 built, 0 already valid" ;;
    usage)
        echo '{"day":"2026-10-30","harness":"claude_code","tool_denials":0,"repeated":0}'
        echo '{"day":"2026-11-01","harness":"codex","tool_denials":0,"repeated":0}'
        echo '{"day":"2026-11-02","harness":"codex","tool_denials":0,"repeated":0}'
        ;;
    install) touch "\$HOME/.install-called" ;;
    *) echo "fake-$tag: \$*" ;;
esac
EOF
    chmod 755 "$path"
}

write_fake old "$install_dir/conbus"
write_fake new "$tmp/stage/conbus-new"
write_fake newer "$tmp/stage/conbus-newer"

# The daemon side of the verdict's window: a stamp that says the swap's own
# daemon started at 10:00, and a daemon.log whose only post-swap start line
# is that boot's own.
cat >"$home/.conbus/daemon.build" <<'EOF'
{"build":"x","pid":4242,"started":"2026-11-01T10:00:00.000Z","serving":"2026-11-01T10:00:04.000Z"}
EOF
cat >"$home/.conbus/daemon.log" <<'EOF'
2026-10-30T09:00:00.000Z conbus daemon: oldbuild: socket /x/sock log /x/log (10 record(s), 0 unreadable)
2026-11-01T10:00:04.000Z conbus daemon: x: socket /x/sock log /x/log (12 record(s), 0 unreadable)
EOF
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
{"ts":"2026-11-01T11:00:00.000Z","reason":"socket closed early"}
EOF
touch "$home/Library/LaunchAgents/sh.conbus.daemon.plist"
for i in 1 2 3 4 5; do touch "$tmp/evidence/s$i.txt"; done

run() {
    env -u CONBUS_HOME -u XDG_STATE_HOME \
        HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 CONBUS_CANARY_REHEARSAL=1 \
        bash "$canary" "$@"
}

# Same fake HOME without the rehearsal declaration — only --live can still
# unlock it.
run_norehearsal() {
    env -u CONBUS_HOME -u XDG_STATE_HOME \
        HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
        bash "$canary" "$@"
}

expect() { # expect <want> <got> <what>
    if [ "$1" != "$2" ]; then
        echo "rehearsal: FAIL: $3 — want $1, got $2" >&2
        exit 1
    fi
    echo "rehearsal: ok: $3"
}

expect_file() { # the file exists
    if [ ! -f "$1" ]; then
        echo "rehearsal: FAIL: $2" >&2
        exit 1
    fi
    echo "rehearsal: ok: $2"
}

expect_absent() { # the path is gone
    if [ -e "$1" ]; then
        echo "rehearsal: FAIL: $2" >&2
        exit 1
    fi
    echo "rehearsal: ok: $2"
}

# The read-only subcommands need no rehearsal declaration.
run_norehearsal status >/dev/null
expect 0 $? "status is read-only"
run preflight >/dev/null
expect 0 $? "preflight passes on a healthy fake install"
run preflight --evidence "$tmp/evidence" >"$tmp/preflight.out"
expect 0 $? "preflight passes with --evidence"
grep -q '^PASS evidence-s5' "$tmp/preflight.out"
expect 0 $? "s1–s5 markers found in --evidence dir"
expect_file "$home/Library/LaunchAgents/sh.conbus.daemon.plist.bak" \
    "preflight backed the plist up beside itself"

# A daemon stamp that disagrees with the binary fails preflight.
cat >"$install_dir/conbus" <<'EOF'
#!/bin/sh
case "$1" in
    --version) echo "conbus 0.0.0-bad" ;;
    doctor)
        echo "socket    $HOME/.conbus/conbus.sock — listening"
        echo "daemon    build aaaa pid 42 since 2026-11-01T00:00:00Z — DIFFERS from this binary (bbbb); restart the daemon"
        echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"
        ;;
    log) echo "cache     0 closed segment(s): 0 built, 0 already valid" ;;
    *) exit 0 ;;
esac
EOF
chmod 755 "$install_dir/conbus"
code=0
run preflight >/dev/null 2>&1 || code=$?
expect 1 "$code" "preflight fails on DIFFERS"
write_fake old "$install_dir/conbus"

# The guard: without real HOME and with launchctl skipped, the swap needs
# the rehearsal declaration — or --live.
code=0
run_norehearsal start --from "$tmp/stage/conbus-new" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start without rehearsal env or --live refuses (exit 2)"
code=0
run_norehearsal rollback >/dev/null 2>&1 || code=$?
expect 2 "$code" "rollback without rehearsal env or --live refuses (exit 2)"

# A blackout refuses start: an epoch window covering now…
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="1000000000-9999999999" \
    bash "$canary" start --from "$tmp/stage/conbus-new" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start refuses inside an epoch blackout"
# …a daily HH:MM-HH:MM window covering now (wrap-safe: the window is an
# hour either side of the current minute)…
lo=$(date -v-1H +%H:%M 2>/dev/null || date -d '1 hour ago' +%H:%M)
hi=$(date -v+1H +%H:%M 2>/dev/null || date -d '+1 hour' +%H:%M)
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="$lo-$hi" \
    bash "$canary" start --from "$tmp/stage/conbus-new" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start refuses inside an HH:MM-HH:MM blackout"
# …and a spec that parses as neither form counts as active (fail safe).
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="unparseable" \
    bash "$canary" start --from "$tmp/stage/conbus-new" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start refuses on an unparseable blackout spec"
# A past window does not block.
run_out=$(env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="1000000000-1000000001" \
    bash "$canary" start --from "$tmp/stage/conbus-new" 2>&1)
expect "conbus 0.0.0-new" "$("$install_dir/conbus" --version)" "a past blackout does not block the swap"

echo "$run_out" | grep -q "install --apply.*skipped\|skipped.*install --apply"
expect 0 $? "rehearsal prints that install --apply was skipped"
expect_absent "$home/.install-called" "rehearsal never invoked install --apply"
expect "conbus 0.0.0-old" "$("$rollback_bin" --version)" "conbus.rollback kept the replaced binary"
expect_absent "$install_dir/conbus.new" "write-beside temp is renamed, not left"

# --live is the alias for the rehearsal declaration.
run_norehearsal start --from "$tmp/stage/conbus-newer" --live >/dev/null
expect "conbus 0.0.0-newer" "$("$install_dir/conbus" --version)" "start with --live and no rehearsal env"
expect "conbus 0.0.0-old" "$("$rollback_bin" --version)" "conbus.rollback still holds the first baseline"

# rollback restores the original; the rollback copy is consumed.
run rollback >/dev/null
expect 0 $? "rollback"
expect "conbus 0.0.0-old" "$("$install_dir/conbus" --version)" "rollback restored the original"
expect_absent "$rollback_bin" "conbus.rollback is consumed by the mv back"

# rollback with no conbus.rollback refuses.
code=0
run rollback >/dev/null 2>&1 || code=$?
expect 1 "$code" "rollback refuses when conbus.rollback is missing"

# start also refuses a candidate that is not there.
code=0
run start --from "$tmp/stage/nonexistent" >/dev/null 2>&1 || code=$?
expect 1 "$code" "start refuses a missing candidate"

# verdict scores the window: all criteria PASS or UNCHECKED here.
code=0
run verdict >"$tmp/verdict.out" 2>&1 || code=$?
expect 0 "$code" "verdict exits 0 on a clean window"
for name in restarts unreadable drops harness-activity twins-repeated policy sync \
    usage-parity rss-budget operator-feel; do
    grep -qE "^(PASS|FAIL|UNCHECKED) $name " "$tmp/verdict.out"
    expect 0 $? "verdict prints a $name line"
done
grep -q '^PASS restarts' "$tmp/verdict.out"
expect 0 $? "0 unplanned restarts (the stamp's own boot is not counted)"
grep -q '^PASS drops' "$tmp/verdict.out"
expect 0 $? "0 new drop reasons"

# A drop reason first seen after the swap fails the verdict.
cat >>"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-11-01T12:00:00.000Z","reason":"a brand new fault"}
EOF
code=0
run verdict >"$tmp/verdict-fail.out" 2>&1 || code=$?
expect 1 "$code" "verdict exits 1 on a new drop reason"
grep -q '^FAIL drops' "$tmp/verdict-fail.out"
expect 0 $? "the new reason is named FAIL"

echo "rehearsal: all checks passed"
