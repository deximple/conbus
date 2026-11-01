#!/bin/bash
# Rehearse canary.sh end-to-end under a throwaway HOME: a fake install dir,
# fake conbus binaries, CONBUS_SKIP_LAUNCHCTL=1 so the launchctl step is
# skipped but the file swap still runs. Nothing here can touch the real
# ~/.local/bin or ~/.conbus — HOME is a mktemp dir for every invocation.
#
#   scripts/canary-rehearsal.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
canary=$here/canary.sh

tmp=$(mktemp -d "${TMPDIR:-/tmp}/canary-rehearsal.XXXXXX")
trap 'rm -rf "$tmp"' EXIT

home=$tmp/home
install_dir=$home/.local/bin
mkdir -p "$install_dir" "$tmp/stage"

# A fake conbus: a version, a doctor with a not-answering socket (the absent
# daemon is the expected rehearsal state), and a `log index` that succeeds.
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
        ;;
    log) echo "cache     0 closed segment(s): 0 built, 0 already valid" ;;
    *) echo "fake-$tag: \$*" ;;
esac
EOF
    chmod 755 "$path"
}

write_fake old "$install_dir/conbus"
write_fake new "$tmp/stage/conbus-new"
write_fake newer "$tmp/stage/conbus-newer"

run() {
    env -u CONBUS_HOME -u XDG_STATE_HOME \
        HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 bash "$canary" "$@"
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

# The always-safe subcommands run with no --live.
run preflight >/dev/null
expect 0 $? "preflight passes on a healthy fake install"
run status >/dev/null
expect 0 $? "status is read-only and needs no --live"

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

# The guard: the swap subcommands refuse without --live.
code=0
run start --from "$tmp/stage/conbus-new" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start without --live refuses (exit 2)"
code=0
run rollback >/dev/null 2>&1 || code=$?
expect 2 "$code" "rollback without --live refuses (exit 2)"

# start --from + --live: swap happens, previous binary kept.
run start --from "$tmp/stage/conbus-new" --live >/dev/null
expect 0 $? "start --from <new> --live"
expect "conbus 0.0.0-new" "$("$install_dir/conbus" --version)" "installed binary is the candidate"
expect "conbus 0.0.0-old" "$("$install_dir/conbus.prev" --version)" "conbus.prev kept the replaced binary"
expect_absent "$install_dir/conbus.new" "write-beside temp is renamed, not left"

# A second start does not clobber the original .prev.
run start --from "$tmp/stage/conbus-newer" --live >/dev/null
expect "conbus 0.0.0-newer" "$("$install_dir/conbus" --version)" "second swap installs newer"
expect "conbus 0.0.0-old" "$("$install_dir/conbus.prev" --version)" "conbus.prev still holds the first baseline"

# rollback restores the original and keeps .prev.
run rollback --live >/dev/null
expect 0 $? "rollback --live"
expect "conbus 0.0.0-old" "$("$install_dir/conbus" --version)" "rollback restored the original"
expect_file "$install_dir/conbus.prev" "conbus.prev remains after rollback"

# rollback with no .prev refuses.
rm "$install_dir/conbus.prev"
code=0
run rollback --live >/dev/null 2>&1 || code=$?
expect 1 "$code" "rollback refuses when conbus.prev is missing"

# start also refuses a candidate that is not there.
code=0
run start --from "$tmp/stage/nonexistent" --live >/dev/null 2>&1 || code=$?
expect 1 "$code" "start refuses a missing candidate"

echo "rehearsal: all checks passed"
