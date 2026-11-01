#!/bin/bash
# Guarded canary control for this user's installed conbus.
#
#   scripts/canary.sh preflight
#   scripts/canary.sh status
#   scripts/canary.sh start --from <candidate-binary> --live
#   scripts/canary.sh rollback --live
#
# preflight and status only read the real install and the real data root.
# start and rollback are the only subcommands that touch the install, and
# they refuse without --live: an accidental `sh canary.sh start` must do
# nothing. Guard semantics:
#   --live                  unlocks the file swap — nothing else does
#   CONBUS_SKIP_LAUNCHCTL=1 skips only the launchctl step, never the swap
#                           (a rehearsal runs --live + CONBUS_SKIP_LAUNCHCTL
#                           under a throwaway HOME)
#
# The install dir is detected exactly the way install.sh places it:
# $HOME/.local/bin. Swaps write beside the target and rename — never a copy
# over a binary a running daemon has mapped.
set -euo pipefail

say() { printf 'canary: %s\n' "$*"; }
die() { say "$*" >&2; exit 1; }
refuse() { say "$*" >&2; exit 2; }

install_dir=$HOME/.local/bin
daemon_label=sh.conbus.daemon
bin=$install_dir/conbus
prev=$install_dir/conbus.prev
new=$install_dir/conbus.new

# Every check must describe the install and data root the managed daemon
# sees. launchd never inherits a shell's CONBUS_HOME/XDG_STATE_HOME, so
# neither does this script.
unset CONBUS_HOME XDG_STATE_HOME
root=$HOME/.conbus
stamp=$root/daemon.build

require_live() {
    [ "${live:-0}" = 1 ] ||
        refuse "$1 refuses without --live — that flag is the only thing that unlocks the real install"
}

# launchctl kickstart -k on the GUI-domain service — but never over a
# missing tool or a declared rehearsal. Returns 0 when it restarted.
restart_daemon() {
    if [ -n "${CONBUS_SKIP_LAUNCHCTL:-}" ]; then
        say "CONBUS_SKIP_LAUNCHCTL set — restart the daemon yourself:"
        say "  launchctl kickstart -k gui/$(id -u)/$daemon_label"
        return 1
    fi
    if ! command -v launchctl >/dev/null 2>&1; then
        say "no launchctl on this system — restart the daemon yourself"
        say "  (systemd: systemctl --user restart $daemon_label)"
        return 1
    fi
    if launchctl kickstart -k "gui/$(id -u)/$daemon_label"; then
        return 0
    fi
    say "launchctl kickstart failed — is the service bootstrapped? ('conbus install --apply' registers it)"
    return 1
}

# Wait for the daemon stamp to show a different pid serving, then let doctor
# have the last word. The stamp is a small file; doctor scans the whole log,
# so it runs once, after the wait — not inside it.
wait_for_serving() {
    old_pid=$1
    n=0
    while [ $n -lt 60 ]; do
        if [ -f "$stamp" ]; then
            pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
            if [ -n "$pid" ] && [ "$pid" != "$old_pid" ] &&
                grep -q '"serving"' "$stamp"; then
                return 0
            fi
        fi
        sleep 0.5
        n=$((n + 1))
    done
    return 1
}

# Report the socket state and fail (1) only when a restart we made did not
# come up; when the restart was skipped, not-listening is expected.
check_doctor() {
    restarted=$1
    doctor_out=$("$bin" doctor 2>&1 || true)
    echo "$doctor_out" | grep -E '^(socket|daemon|recorder|hardening)' || true
    if echo "$doctor_out" | grep -q 'listening'; then
        say "doctor: socket listening"
        return 0
    fi
    if [ "$restarted" = 1 ]; then
        say "doctor: the daemon is not answering after kickstart —"
        say "  check \`canary.sh status\`, or roll back: canary.sh rollback --live"
        return 1
    fi
    say "doctor: not answering — expected until the daemon is restarted"
    return 0
}

sub=${1:-}
[ $# -ge 1 ] && shift
live=0
from=""
while [ $# -gt 0 ]; do
    case $1 in
        --live) live=1 ;;
        --from)
            [ $# -ge 2 ] || refuse "--from wants a path"
            from=$2
            shift
            ;;
        *) refuse "unknown argument: $1" ;;
    esac
    shift
done

case $sub in
    preflight)
        fail=0
        check() { # check <ok> <name> <detail>
            if [ "$1" = 0 ]; then
                printf 'PASS %s %s\n' "$2" "$3"
            else
                printf 'FAIL %s %s\n' "$2" "$3"
                fail=1
            fi
        }

        [ -x "$bin" ] || die "no installed conbus at $bin — nothing to canary"
        version=$("$bin" --version 2>&1 || true)
        if [ -n "$version" ]; then
            check 0 version "$version"
        else
            check 1 version "no output"
        fi

        doctor_out=$("$bin" doctor 2>&1 || true)
        socket_line=$(echo "$doctor_out" | grep '^socket' || true)
        case $socket_line in
            *listening* | *"not answering"*)
                check 0 socket "${socket_line#socket    }"
                ;;
            *)
                check 1 socket "${socket_line:-no socket line}"
                ;;
        esac
        disk_line=$(echo "$doctor_out" | grep '^disk' || true)
        case $disk_line in
            *WARNING*) check 1 disk "$disk_line" ;;
            *) check 0 disk "${disk_line:-no disk line}" ;;
        esac
        if echo "$doctor_out" | grep -q 'DIFFERS'; then
            check 1 daemon-build "$(echo "$doctor_out" | grep 'DIFFERS' | head -1)"
        else
            check 0 daemon-build "no DIFFERS"
        fi

        if index_out=$("$bin" log index 2>&1); then
            check 0 cache "$index_out"
        else
            check 1 cache "conbus log index failed: $index_out"
        fi
        exit "$fail"
        ;;

    status)
        if [ -x "$bin" ]; then
            say "installed: $("$bin" --version 2>&1 || echo 'does not run')"
        else
            say "installed: no conbus at $bin"
        fi
        if [ -f "$prev" ]; then
            say "previous: $("$prev" --version 2>&1 || echo 'present, does not run')"
        else
            say "previous: none ($prev absent)"
        fi
        if [ -f "$stamp" ]; then
            say "daemon stamp: $(cat "$stamp")"
        else
            say "daemon stamp: none"
        fi
        if [ -x "$bin" ]; then
            socket_line=$("$bin" doctor 2>/dev/null | grep '^socket' || true)
            say "socket: ${socket_line:-doctor gave no socket line}"
        elif [ -S "$root/conbus.sock" ]; then
            say "socket: $root/conbus.sock exists"
        else
            say "socket: none"
        fi
        ;;

    start)
        require_live start
        [ -n "$from" ] || refuse "start wants --from <candidate-binary>"
        [ -x "$from" ] || die "candidate is not an executable: $from"
        [ -x "$bin" ] || die "no installed conbus at $bin — canary only covers an install.sh install"
        cand_version=$("$from" --version 2>&1 || true)
        [ -n "$cand_version" ] || die "candidate does not run: $from"
        old_version=$("$bin" --version 2>&1 || echo unknown)
        old_stamp_pid=""
        [ -f "$stamp" ] && old_stamp_pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")

        if [ ! -f "$prev" ]; then
            cp -p "$bin" "$prev"
            say "kept the current binary at $prev"
        fi
        # Write beside, then rename: a daemon holding the old inode is never
        # cut off mid-map.
        cp "$from" "$new"
        chmod 755 "$new"
        mv -f "$new" "$bin"
        say "swapped $bin: $old_version -> $cand_version"

        restarted=0
        if restart_daemon; then restarted=1; fi
        if [ "$restarted" = 1 ]; then wait_for_serving "$old_stamp_pid" || true; fi
        check_doctor "$restarted"
        ;;

    rollback)
        require_live rollback
        [ -f "$prev" ] ||
            die "no $prev — nothing to roll back to; start keeps one before the first swap"
        old_stamp_pid=""
        [ -f "$stamp" ] && old_stamp_pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
        # Write beside, then rename; conbus.prev stays for forensics.
        cp "$prev" "$new"
        chmod 755 "$new"
        mv -f "$new" "$bin"
        say "rolled back $bin to $("$bin" --version 2>&1 || echo unknown)"

        restarted=0
        if restart_daemon; then restarted=1; fi
        if [ "$restarted" = 1 ]; then wait_for_serving "$old_stamp_pid" || true; fi
        check_doctor "$restarted"
        ;;

    "" | -h | --help | help)
        sed -n '3,8p' "$0"
        ;;
    *)
        refuse "unknown subcommand: $sub (preflight|status|start|rollback)"
        ;;
esac
