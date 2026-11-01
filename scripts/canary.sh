#!/bin/bash
# Guarded canary control for this user's installed conbus.
#
#   scripts/canary.sh preflight [--evidence DIR]
#   scripts/canary.sh status
#   scripts/canary.sh verdict [--since <ts>]
#   scripts/canary.sh start --from <candidate-binary>
#   scripts/canary.sh restart
#   scripts/canary.sh rollback
#
# preflight, status and verdict are read-only (preflight also makes the
# plist backup — a copy, never a change). start, restart and rollback swap
# or reload the installed binary. Per the 1.0 plan, §7:
#   - the swap is real only under the real HOME with CONBUS_SKIP_LAUNCHCTL
#     unset; it then ends in `conbus install --apply` (bootout + bootstrap,
#     never kickstart — the stamp-poll and doctor still run after it);
#   - anything else is a rehearsal and needs CONBUS_CANARY_REHEARSAL=1
#     (--live is a convenience alias): the file swap still runs, launchctl
#     and `install --apply` are skipped and the script says so;
#   - a start during CONBUS_CANARY_BLACKOUT is refused. The spec is
#     HH:MM-HH:MM (daily, wrap allowed), `[days] HH:MM-HH:MM [Zone]` like
#     "Mon-Fri 09:00-15:30 Asia/Seoul", or <epoch>-<epoch>; a spec that
#     parses as none of these counts as an active blackout — fail safe —
#     and the refusal says the spec was unreadable;
#   - start runs preflight first and refuses before swapping when it
#     fails. After the swap it requires, inside
#     CONBUS_CANARY_SERVING_LIMIT seconds (default 30): a new daemon pid
#     serving, a doctor with no DIFFERS against the candidate build,
#     every hooks line `runs`, and zero drop reasons that were not there
#     before. Any failure rolls back automatically and exits 1. The
#     5-second serving goal is a verdict criterion, not a rollback one —
#     current measurements sit above it and would undo every cutover.
#   - state lives at <root>/canary.json: swap time, previous/candidate
#     builds, candidate sha, and the planned restarts (each with its
#     start->serving milliseconds and new-drop count).
#   - `restart` performs a planned restart (`install --apply`) and records
#     its measurements. `verdict` bounds its window by the recorded swap
#     time unless --since overrides it.
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
rollback_bin=$install_dir/conbus.rollback
new_bin=$install_dir/conbus.new

# Every check must describe the install and data root the managed daemon
# sees. launchd never inherits a shell's CONBUS_HOME/XDG_STATE_HOME, so
# neither does this script.
unset CONBUS_HOME XDG_STATE_HOME
root=$HOME/.conbus
stamp=$root/daemon.build
daemon_log=$root/daemon.log
drops_log=$root/drops.jsonl
state_file=$root/canary.json

serving_limit=${CONBUS_CANARY_SERVING_LIMIT:-30}
rss_budget_mb=${CONBUS_CANARY_RSS_MB:-600}
window_days=${CONBUS_CANARY_WINDOW_DAYS:-7}

is_uint() { case $1 in '' | *[!0-9]*) return 1 ;; *) return 0 ;; esac }

# The real environment: HOME is this user's passwd home and nothing asked
# that launchctl be skipped. Anything else must declare itself a rehearsal.
is_real_env() {
    [ -z "${CONBUS_SKIP_LAUNCHCTL:-}" ] || return 1
    real_home=$(env -u HOME bash -c 'cd && pwd -P' 2>/dev/null) || return 1
    [ "$(cd "$HOME" && pwd -P)" = "$real_home" ]
}

# --- blackout ---------------------------------------------------------------

# Weekday spec like Mon, Mon-Fri, Sat,Sun — 0 if it parses.
is_day_spec() {
    case $1 in
        Mon | Tue | Wed | Thu | Fri | Sat | Sun) return 0 ;;
    esac
    case $1 in
        Mon- | Tue- | Wed- | Thu- | Fri- | Sat- | Sun- | \
        *-Mon | *-Tue | *-Wed | *-Thu | *-Fri | *-Sat | *-Sun) : ;;
        *) return 1 ;;
    esac
    rest=$1
    while :; do
        case $rest in
            Mon | Tue | Wed | Thu | Fri | Sat | Sun) return 0 ;;
            Mon-* | Tue-* | Wed-* | Thu-* | Fri-* | Sat-* | Sun-*)
                rest=${rest#*-} ;;
            Mon,* | Tue,* | Wed,* | Thu,* | Fri,* | Sat,* | Sun,*)
                rest=${rest#*,} ;;
            *) return 1 ;;
        esac
    done
}

# Map an English weekday name to date +%u (Mon=1 .. Sun=7).
day_num() {
    case $1 in
        Mon) echo 1 ;; Tue) echo 2 ;; Wed) echo 3 ;; Thu) echo 4 ;;
        Fri) echo 5 ;; Sat) echo 6 ;; Sun) echo 7 ;;
        *) return 1 ;;
    esac
}

# 0 if today's weekday (1..7) is inside the day spec.
day_matches() {
    spec=$1 today=$2
    # Ranges first: X-Y
    case $spec in
        *-*) lo=${spec%%-*}; hi=${spec##*-}
            lo_n=$(day_num "$lo") hi_n=$(day_num "$hi") || return 1
            if [ "$lo_n" -le "$hi_n" ]; then
                [ "$today" -ge "$lo_n" ] && [ "$today" -le "$hi_n" ]
            else
                [ "$today" -ge "$lo_n" ] || [ "$today" -le "$hi_n" ]
            fi
            return ;;
    esac
    old_ifs=$IFS; IFS=,
    for d in $spec; do
        [ "$(day_num "$d" 2>/dev/null)" = "$today" ] && { IFS=$old_ifs; return 0; }
    done
    IFS=$old_ifs
    return 1
}

# A timezone name we can put after TZ= : a zoneinfo path, or a known short
# name. Anything else is unparseable — better refused than silently local.
is_tz_name() {
    case $1 in
        */*) [ -f "/usr/share/zoneinfo/$1" ] ;;
        UTC | GMT | localtime | Localtime) return 0 ;;
        *) return 1 ;;
    esac
}

# In-window check for "HH:MM-HH:MM" given now as HH:MM — wrap allowed.
in_time_range() {
    s=${1%%-*} e=${1##*-} now=$2
    [ "$s" = "$e" ] && return 0
    if [ "$s" \< "$e" ]; then
        [ ! "$now" \< "$s" ] && [ "$now" \< "$e" ]
    else
        [ ! "$now" \< "$s" ] || [ "$now" \< "$e" ]
    fi
}

is_time_range() {
    case $1 in
        [0-9][0-9]:[0-9][0-9]-[0-9][0-9]:[0-9][0-9]) : ;;
        *) return 1 ;;
    esac
    s=${1%%-*} e=${1##*-}
    sh=${s%%:*} sm=${s##*:} eh=${e%%:*} em=${e##*:}
    is_uint "$sh" && is_uint "$sm" && is_uint "$eh" && is_uint "$em" || return 1
    [ "$sh" -le 23 ] && [ "$sm" -le 59 ] && [ "$eh" -le 23 ] && [ "$em" -le 59 ]
}

# Is a blackout window active right now? 0 = active, 1 = not. An unset spec
# is never active; a spec that parses as no known form counts as active —
# an unreadable window must not let a swap through — and BLACKOUT_REASON
# says so.
BLACKOUT_REASON=""
blackout_active() {
    spec=${CONBUS_CANARY_BLACKOUT:-}
    [ -z "$spec" ] && return 1
    days="" range="" tz=""
    set -- $spec
    case $# in
        1) range=$1 ;;
        2)
            if is_day_spec "${1:-}"; then days=$1 range=$2
            else range=$1 tz=$2
            fi
            ;;
        3) days=$1 range=$2 tz=$3 ;;
        *) BLACKOUT_REASON="unparseable"; return 0 ;;
    esac
    if is_time_range "$range"; then
        if [ -n "$tz" ] && ! is_tz_name "$tz"; then
            BLACKOUT_REASON="unparseable"
            return 0
        fi
        tzone=${tz:-}
        now_hm=$(env ${tzone:+TZ="$tzone"} date +%H:%M)
        if [ -n "$days" ]; then
            today=$(env ${tzone:+TZ="$tzone"} date +%u)
            day_matches "$days" "$today" || return 1
        fi
        in_time_range "$range" "$now_hm"
        return
    fi
    case $spec in
        *-*)
            a=${spec%%-*} b=${spec##*-}
            is_uint "$a" && is_uint "$b" || { BLACKOUT_REASON="unparseable"; return 0; }
            [ "$a" -lt "$b" ] || { BLACKOUT_REASON="unparseable"; return 0; }
            now=$(date +%s)
            [ "$now" -ge "$a" ] && [ "$now" -lt "$b" ]
            ;;
        *) BLACKOUT_REASON="unparseable"; return 0 ;;
    esac
}

# --- daemon ---------------------------------------------------------------

# Decide real vs rehearsal for the swap subcommands. Echoes the mode;
# refuses when neither applies.
swap_mode() {
    sub=$1
    if is_real_env; then
        echo real
        return
    fi
    if [ -n "${CONBUS_CANARY_REHEARSAL:-}" ] || [ "${live:-0}" = 1 ]; then
        echo rehearsal
        return
    fi
    refuse "$sub: not the real HOME or CONBUS_SKIP_LAUNCHCTL is set —" \
        "rehearsing needs CONBUS_CANARY_REHEARSAL=1 (or --live); the file" \
        "swap then runs but launchctl and install --apply are skipped"
}

# After a swap: re-register hooks and reload the daemon. In the real path
# `conbus install --apply` does both (bootout + bootstrap, per HANDOFF);
# a rehearsal skips it because it would call launchctl.
reregister_daemon() {
    mode=$1
    if [ "$mode" = rehearsal ]; then
        say "rehearsal: 'conbus install --apply' skipped (it would hit launchctl)"
        return 1
    fi
    if "$bin" install --apply; then
        return 0
    fi
    say "conbus install --apply failed — check 'canary.sh status'"
    return 1
}

# Wait for the daemon stamp to show a different pid serving. The limit is
# CONBUS_CANARY_SERVING_LIMIT seconds (default 30). Failure is meaningful —
# callers decide whether it rolls back.
wait_for_serving() {
    old_pid=$1
    n=0
    limit=$((serving_limit * 2))
    while [ $n -lt "$limit" ]; do
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

# Report the socket state and fail (1) only when a real reload did not come
# up; in rehearsal, not-listening is expected.
check_doctor() {
    mode=$1
    doctor_out=$("$bin" doctor 2>&1 || true)
    echo "$doctor_out" | grep -E '^(socket|daemon|recorder|hardening)' || true
    if echo "$doctor_out" | grep -q 'DIFFERS'; then
        say "doctor: the answering daemon is a different build —"
        say "  check \`canary.sh status\`, or roll back: canary.sh rollback"
        return 1
    fi
    if echo "$doctor_out" | grep -q 'listening'; then
        say "doctor: socket listening"
        return 0
    fi
    if [ "$mode" = real ]; then
        say "doctor: the daemon is not answering after install --apply —"
        say "  check \`canary.sh status\`, or roll back: canary.sh rollback"
        return 1
    fi
    say "doctor: not answering — expected while the daemon is not restarted"
    return 0
}

# --- canary.json ------------------------------------------------------------

state_get() { # state_get <dotted key> — flat fields only
    [ -f "$state_file" ] || return 1
    sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p" "$state_file" | head -1
}

state_restart_count() {
    [ -f "$state_file" ] || { echo 0; return; }
    { grep -o '"serving_ms"' "$state_file" || true; } | wc -l | tr -d ' '
}

state_restart_drop_total() {
    [ -f "$state_file" ] || { echo 0; return; }
    { grep -o '"new_drops":[0-9]*' "$state_file" || true; } | cut -d: -f2 |
        awk '{ t += $1 } END { print t + 0 }'
}

state_write() { # state_write <swapped> <prev> <cand> <sha> <restarts-json-array> [rolled_back]
    mkdir -p "$root"
    tmp=$state_file.tmp
    printf '{"swapped":"%s","prev_build":"%s","candidate_build":"%s","candidate_sha256":"%s","restarts":%s%s}\n' \
        "$1" "$2" "$3" "$4" "$5" "${6:+,\"rolled_back\":\"$6\"}" >"$tmp"
    mv -f "$tmp" "$state_file"
}

state_restarts_json() { # echoes the restarts array as stored, [] when absent
    [ -f "$state_file" ] || { echo '[]'; return; }
    sed -n 's/.*"restarts":\(\[.*\]\).*/\1/p' "$state_file" | head -1
}

state_add_restart() { # state_add_restart <ts> <serving_ms|-> <new_drops>
    restarts=$(state_restarts_json)
    entry=$(printf '{"ts":"%s","serving_ms":%s,"new_drops":%s}' "$1" "${2:--1}" "${3:-0}")
    if [ "$restarts" = '[]' ]; then
        new="[$entry]"
    else
        new="${restarts%\]},$entry]"
    fi
    state_write "$(state_get swapped)" "$(state_get prev_build)" \
        "$(state_get candidate_build)" "$(state_get candidate_sha256)" "$new"
}

# The distinct drop reasons in drops.jsonl, one per line.
drop_reasons() {
    [ -f "$drops_log" ] || return 0
    sed -n 's/.*"reason":"\([^"]*\)".*/\1/p' "$drops_log" | LC_ALL=C sort -u
}

# Reasons in the second file/list that the first did not have.
# new_drop_reasons <before-file> — prints nothing when there is no diff.
new_drop_reasons() {
    tmp=$(mktemp)
    drop_reasons >"$tmp"
    comm -13 "$1" "$tmp"
    rm -f "$tmp"
}

# YYYY-MM-DD -> the next YYYY-MM-DD, on macOS or GNU date.
next_day() {
    if [ "$(uname -s)" = Darwin ]; then
        date -j -v+1d -f "%Y-%m-%d" "$1" +%Y-%m-%d 2>/dev/null
    else
        date -d "$1 +1 day" +%Y-%m-%d 2>/dev/null
    fi
}

# ISO-8601 UTC -> epoch, on macOS or GNU date. Empty on failure.
ts_epoch() {
    iso=${1%%.*}
    if [ "$(uname -s)" = Darwin ]; then
        date -j -f "%Y-%m-%dT%H:%M:%S" "$iso" +%s 2>/dev/null || true
    else
        date -d "$iso" +%s 2>/dev/null || true
    fi
}

# --- subcommands ------------------------------------------------------------

do_preflight() {
    fail=0
    check() { # check <level: 0=PASS 1=FAIL 2=WARN> <name> <detail>
        case $1 in
            0) printf 'PASS %s %s\n' "$2" "$3" ;;
            2) printf 'WARN %s %s\n' "$2" "$3" ;;
            *)
                printf 'FAIL %s %s\n' "$2" "$3"
                fail=$((fail + 1))
                ;;
        esac
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

    # S1–S5 evidence, kept outside the repo per the plan. With no
    # --evidence the default ./evidence is only warned about — the
    # operator's evidence may live somewhere else.
    evidence_dir=${evidence:-./evidence}
    if [ -d "$evidence_dir" ]; then
        for i in 1 2 3 4 5; do
            found=$(cd "$evidence_dir" && { ls "s$i"* "S$i"* 2>/dev/null || true; } | head -1)
            if [ -n "$found" ]; then
                check 0 "evidence-s$i" "$evidence_dir/$found"
            elif [ -n "$evidence" ]; then
                check 1 "evidence-s$i" "missing in $evidence_dir"
            else
                check 2 "evidence-s$i" "missing in $evidence_dir (default dir — warn only)"
            fi
        done
    elif [ -n "$evidence" ]; then
        check 1 evidence "directory not found: $evidence_dir"
    else
        check 2 evidence "no $evidence_dir — pass --evidence DIR when it lives elsewhere"
    fi

    # The rollback copy: present already, or makeable from the install.
    if [ -f "$rollback_bin" ]; then
        check 0 rollback-copy "$rollback_bin"
    elif [ -x "$bin" ]; then
        check 0 rollback-copy "absent — will be made from $bin at start"
    else
        check 1 rollback-copy "no $rollback_bin and no $bin to copy"
    fi

    # The launchd plist (or systemd unit on Linux), backed up beside
    # itself so a bad merge has an undo. Under a rehearsal HOME the file
    # is simply absent.
    if [ "$(uname -s)" = Darwin ]; then
        service_file=$HOME/Library/LaunchAgents/$daemon_label.plist
    else
        service_file=$HOME/.config/systemd/user/$daemon_label.service
    fi
    if [ -f "$service_file" ]; then
        if cp "$service_file" "$service_file.bak"; then
            check 0 plist-backup "$service_file.bak"
        else
            check 1 plist-backup "could not write $service_file.bak"
        fi
    else
        check 0 plist-backup "no service file present — nothing to back up"
    fi

    # The candidate binary: Developer ID signature, hardened runtime on
    # every slice, and a matching published hash when a .sha256 sidecar
    # sits beside it (the release asset naming). macOS only — on Linux
    # the check degrades to the hash.
    if [ -n "$from" ]; then
        [ -f "$from" ] || die "no candidate at $from"
        if [ "$(uname -s)" = Darwin ] && is_real_env; then
            if codesign --verify --strict "$from" 2>/dev/null; then
                team=$(codesign -dv "$from" 2>&1 | sed -n 's/^TeamIdentifier=//p')
                if [ -n "$team" ] && [ "$team" != "not set" ]; then
                    check 0 signature "Developer ID team $team"
                else
                    check 1 signature "verifies but names no team — not a Developer ID build"
                fi
            else
                check 1 signature "codesign --verify --strict failed on $from"
            fi
            slices=$(lipo -archs "$from" 2>/dev/null || uname -m)
            runtime_fail=""
            for arch in $slices; do
                codesign -dv --arch "$arch" "$from" 2>&1 | grep -q 'runtime' ||
                    runtime_fail="$runtime_fail $arch"
            done
            if [ -z "$runtime_fail" ]; then
                check 0 hardened-runtime "all slices ($slices)"
            else
                check 1 hardened-runtime "missing on:$runtime_fail"
            fi
        elif [ "$(uname -s)" = Darwin ]; then
            check 2 signature "rehearsal — signature and runtime checks skipped for a fake binary"
        fi
        if [ -f "$from.sha256" ]; then
            want=$(awk '{print $1}' "$from.sha256")
            got=$(shasum -a 256 "$from" | awk '{print $1}')
            if [ "$want" = "$got" ]; then
                check 0 artifact-hash "matches $from.sha256"
            else
                check 1 artifact-hash "sha256 mismatch: want $want got $got"
            fi
        else
            check 2 artifact-hash "no $from.sha256 beside the candidate"
        fi
    fi

    if blackout_active; then
        if [ "$BLACKOUT_REASON" = unparseable ]; then
            check 1 blackout "spec '${CONBUS_CANARY_BLACKOUT:-}' is unparseable — treated as active"
        else
            check 1 blackout "CONBUS_CANARY_BLACKOUT is active (${CONBUS_CANARY_BLACKOUT:-})"
        fi
    else
        check 0 blackout "no active blackout"
    fi

    # The derived cache, built now by the candidate on the live root, so
    # its first reads do not pay first-build cost at cutover. `log index`
    # only writes cache/ — a rebuildable sidecar — next to the log.
    if [ -n "$from" ]; then
        if index_out=$("$from" log index 2>&1); then
            check 0 cache "$index_out"
        else
            check 1 cache "candidate log index failed: $index_out"
        fi
    else
        check 2 cache "no --from — the live cache is not prebuilt"
    fi
    return "$fail"
}

sub=${1:-}
[ $# -ge 1 ] && shift
live=0
from=""
evidence=""
since=""
while [ $# -gt 0 ]; do
    case $1 in
        --live) live=1 ;;
        --from)
            [ $# -ge 2 ] || refuse "--from wants a path"
            from=$2
            shift
            ;;
        --evidence)
            [ $# -ge 2 ] || refuse "--evidence wants a directory"
            evidence=$2
            shift
            ;;
        --since)
            [ $# -ge 2 ] || refuse "--since wants a timestamp"
            since=$2
            shift
            ;;
        *) refuse "unknown argument: $1" ;;
    esac
    shift
done

case $sub in
    preflight)
        do_preflight
        exit $?
        ;;

    status)
        if [ -x "$bin" ]; then
            say "installed: $("$bin" --version 2>&1 || echo 'does not run')"
        else
            say "installed: no conbus at $bin"
        fi
        if [ -f "$rollback_bin" ]; then
            say "rollback: $("$rollback_bin" --version 2>&1 || echo 'present, does not run')"
        else
            say "rollback: none ($rollback_bin absent)"
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
        if [ -f "$state_file" ]; then
            say "canary: $(cat "$state_file")"
        fi
        ;;

    verdict)
        # The §7 pass criteria a machine can score, against the log since
        # the recorded swap. The default boundary is the swap time in
        # canary.json — restarts of the recorder itself do not move it, so
        # an unplanned one is still counted. Exits 1 on a fail, 3 while the
        # 7-day window is still open, 0 on a completed pass.
        fails=0
        line() { # line <PASS|FAIL|UNCHECKED> <name> <detail>
            printf '%s %s — %s\n' "$1" "$2" "$3"
            if [ "$1" = FAIL ]; then fails=$((fails + 1)); fi
        }

        if [ -z "$since" ]; then
            since=$(state_get swapped || true)
        fi
        if [ -z "$since" ] && [ -f "$stamp" ]; then
            since=$(sed -n 's/.*"started":"\([^"]*\)".*/\1/p' "$stamp")
            say "note: no canary.json — bounding by the daemon's own start $since"
        fi
        [ -n "$since" ] ||
            die "no --since, no canary.json, no daemon.build stamp — nothing to bound the window with"
        case $since in
            [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) since="$since"T00:00:00.000Z ;;
        esac
        since_day=${since:0:10}
        say "window: since $since"

        [ -x "$bin" ] || die "no installed conbus at $bin — verdict needs its reads"

        # 1. Unplanned recorder restarts: daemon start lines after since,
        #    minus the swap's own boot and each planned restart — each of
        #    those is one `install --apply`, one boot.
        starts=0
        if [ -f "$daemon_log" ]; then
            starts=$(awk -v s="$since" '
                $1 > s && /conbus daemon: .*: socket .* log / { n++ }
                END { print n + 0 }
            ' "$daemon_log")
        else
            line UNCHECKED restarts "no $daemon_log"
        fi
        planned=$(state_restart_count)
        unplanned=$((starts - 1 - planned))
        [ "$unplanned" -lt 0 ] && unplanned=0
        if [ -f "$daemon_log" ]; then
            if [ "$unplanned" -eq 0 ]; then
                line PASS restarts \
                    "0 unplanned daemon start(s) since $since ($starts seen: 1 swap + $planned planned)"
            else
                line FAIL restarts \
                    "$unplanned unplanned daemon start(s) since $since ($starts seen: 1 swap + $planned planned)"
            fi
        fi

        # 2. Unreadable log lines: the newest start line's report, else the
        #    doctor's log line.
        unreadable=""
        if [ -f "$daemon_log" ]; then
            unreadable=$(sed -n 's/.*(\([0-9][0-9]*\) record(s), \([0-9][0-9]*\) unreadable).*/\2/p' \
                "$daemon_log" | tail -1)
        fi
        if [ -z "$unreadable" ]; then
            unreadable=$("$bin" doctor 2>/dev/null |
                sed -n 's/^log.* \([0-9][0-9]*\) unreadable line(s)/\1/p' | tail -1)
        fi
        case ${unreadable:-x} in
            x) line UNCHECKED unreadable "no report line names a count" ;;
            0) line PASS unreadable "0 unreadable line(s)" ;;
            *) line FAIL unreadable "$unreadable unreadable line(s)" ;;
        esac

        # 3. New drop reasons since the swap (a known reason repeating is
        #    not new), plus every planned restart's recorded new_drops.
        if [ -f "$drops_log" ]; then
            new_reasons=$(awk -v s="$since" '
                {
                    ts = ""; r = ""
                    if (match($0, /"ts":"[^"]*"/)) ts = substr($0, RSTART + 6, RLENGTH - 7)
                    if (match($0, /"reason":"[^"]*"/)) r = substr($0, RSTART + 10, RLENGTH - 11)
                    if (r == "") next
                    if (ts > s) { after[r] = 1; n++ } else { before[r] = 1 }
                }
                END { print n + 0; for (r in after) if (!(r in before)) print r }
            ' "$drops_log")
            drop_count=$(echo "$new_reasons" | head -1)
            new_reasons=$(echo "$new_reasons" | tail -n +2 | LC_ALL=C sort)
            restart_drops=$(state_restart_drop_total)
            if [ -z "$new_reasons" ] && [ "$restart_drops" -eq 0 ]; then
                line PASS drops \
                    "0 new drop reason(s) ($drop_count drop(s), $restart_drops on planned restarts)"
            elif [ -n "$new_reasons" ]; then
                line FAIL drops \
                    "new drop reason(s) since $since: $(echo "$new_reasons" | tr '\n' ' ')"
            else
                line FAIL drops "$restart_drops drop(s) during planned restarts"
            fi
        else
            restart_drops=$(state_restart_drop_total)
            if [ "$restart_drops" -eq 0 ]; then
                line PASS drops "no $drops_log — nothing dropped"
            else
                line FAIL drops "$restart_drops drop(s) during planned restarts"
            fi
        fi

        # 4. Per-harness activity: usage --json rows give every (day,
        #    harness) with records. A harness that misses a day some other
        #    harness recorded on cannot be told idle from lost — UNCHECKED
        #    with the gap named.
        usage_json=$("$bin" usage --json --since 1970-01-01 2>/dev/null || true)
        if [ -z "$usage_json" ]; then
            line UNCHECKED harness-activity "usage --json gave no rows"
        else
            activity=$(echo "$usage_json" | awk -v s="$since_day" '
                {
                    day = ""; h = ""
                    if (match($0, /"day":"[^"]*"/)) day = substr($0, RSTART + 7, RLENGTH - 8)
                    if (match($0, /"harness":"[^"]*"/)) h = substr($0, RSTART + 11, RLENGTH - 12)
                    if (day == "" || h == "") next
                    if (day >= s) {
                        wh[h SUBSEP day] = 1; active[h] = 1; wdays[day] = 1
                    } else {
                        before[h] = 1
                    }
                }
                END {
                    nwdays = 0; for (d in wdays) nwdays++
                    gaps = ""; silent = ""
                    for (h in active) {
                        have = 0
                        for (d in wdays) if (wh[h, d]) have++
                        if (have < nwdays) gaps = gaps " " h
                    }
                    for (h in before) if (!(h in active)) silent = silent " " h
                    printf "gaps:%s\nsilent:%s\ndays:%d\n", gaps, silent, nwdays
                }')
            gaps=$(echo "$activity" | sed -n 's/^gaps: //p')
            silent=$(echo "$activity" | sed -n 's/^silent: //p')
            nwdays=$(echo "$activity" | sed -n 's/^days://p')
            if [ -z "$gaps" ]; then
                line PASS harness-activity \
                    "every active harness recorded on all $nwdays active day(s)${silent:+; silent:$silent}"
            else
                line UNCHECKED harness-activity \
                    "day-gap(s) on:$gaps — idle vs lost needs the operator${silent:+; silent:$silent}"
            fi
        fi

        # 5. twins and `repeated` growth vs the pre-swap baseline, same rows.
        if [ -n "$usage_json" ]; then
            rep_result=$(echo "$usage_json" | awk -v s="$since_day" '
                {
                    day = ""
                    if (match($0, /"day":"[^"]*"/)) day = substr($0, RSTART + 7, RLENGTH - 8)
                    if (match($0, /"repeated":[0-9]+/)) rep = substr($0, RSTART + 11, RLENGTH - 11) + 0
                    else rep = 0
                    if (day >= s) { wr += rep; wd[day] = 1 } else { br += rep; bd[day] = 1 }
                }
                END {
                    wdays = 0; for (d in wd) wdays++
                    bdays = 0; for (d in bd) bdays++
                    wrate = wdays ? wr / wdays : 0
                    brate = bdays ? br / bdays : 0
                    printf "%s %.4f %.4f %d %d", (wrate <= brate ? "PASS" : "FAIL"), wrate, brate, wr, br
                }')
            set -- $rep_result
            line "$1" twins-repeated \
                "repeated/day $2 since vs $3 before ($4 vs $5 total)"
        else
            line UNCHECKED twins-repeated "usage --json gave no rows"
        fi

        # 6. usage-parity: every closed UTC day in the window, the rollback
        #    binary's usage --json --day D against the candidate's, with
        #    the documented tokens_* keys stripped.
        today_utc=$(date -u +%Y-%m-%d)
        if [ ! -x "$rollback_bin" ]; then
            line FAIL usage-parity "no $rollback_bin — the 0.2.8 side of the parity is missing"
        else
            strip='s/"tokens_[a-z_]*":[0-9]*,\?//g; s/,\}/}/g; s/,]/]/g'
            parity_bad=""
            d=$since_day
            guard=0
            while [ "$d" \< "$today_utc" ] && [ "$guard" -lt 370 ]; do
                old=$("$rollback_bin" usage --json --day "$d" 2>/dev/null | sed "$strip" || true)
                new=$("$bin" usage --json --day "$d" 2>/dev/null | sed "$strip" || true)
                if [ "$old" != "$new" ]; then
                    parity_bad="$parity_bad $d"
                fi
                d=$(next_day "$d")
                guard=$((guard + 1))
            done
            if [ -z "$parity_bad" ]; then
                line PASS usage-parity "closed days agree (tokens_* stripped)"
            else
                line FAIL usage-parity "usage differs on:$parity_bad"
            fi
        fi

        # 7. rss-budget + M3 time budgets: the running recorder's RSS and
        #    three re-measured reads against the plan's targets.
        rss_kb=""
        if [ -f "$stamp" ]; then
            pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
            [ -n "$pid" ] && rss_kb=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')
        fi
        if is_uint "${rss_kb:-}"; then
            rss_mb=$((rss_kb / 1024))
            if [ "$rss_mb" -le "$rss_budget_mb" ]; then
                line PASS rss-budget "recorder RSS ${rss_mb} MB ≤ ${rss_budget_mb} MB"
            else
                line FAIL rss-budget "recorder RSS ${rss_mb} MB > ${rss_budget_mb} MB"
            fi
        else
            line FAIL rss-budget "no running recorder pid — the daemon itself is not up"
        fi

        measure() { # measure <cmd...> — echoes wall-clock milliseconds
            start_ms=$(perl -MTime::HiRes=time -e 'printf "%d", time()*1000')
            "$@" >/dev/null 2>&1 || true
            end_ms=$(perl -MTime::HiRes=time -e 'printf "%d", time()*1000')
            echo $((end_ms - start_ms))
        }
        t_tail=$(measure "$bin" log --tail 1)
        t_usage=$(measure "$bin" usage --days 7)
        t_doctor=$(measure "$bin" doctor)
        tb_bad=""
        [ "$t_tail" -gt 200 ] && tb_bad="$tb_bad tail"
        [ "$t_usage" -gt 2000 ] && tb_bad="$tb_bad usage"
        [ "$t_doctor" -gt 3000 ] && tb_bad="$tb_bad doctor"
        if [ -z "$tb_bad" ]; then
            line PASS time-budget \
                "log --tail 1 ${t_tail}ms≤200 usage --days 7 ${t_usage}ms≤2000 doctor ${t_doctor}ms≤3000"
        else
            line FAIL time-budget \
                "log --tail 1 ${t_tail}ms usage --days 7 ${t_usage}ms doctor ${t_doctor}ms — over on:$tb_bad"
        fi

        # 8. Serving ≤ 5 s on the newest recorded boot — the goal §7 moved
        #    here from the rollback checks.
        if [ -f "$stamp" ]; then
            started=$(sed -n 's/.*"started":"\([^"]*\)".*/\1/p' "$stamp")
            serving=$(sed -n 's/.*"serving":"\([^"]*\)".*/\1/p' "$stamp")
            e0=$(ts_epoch "$started") e1=$(ts_epoch "$serving")
            if is_uint "${e0:-}" && is_uint "${e1:-}"; then
                serving_ms=$(( (e1 - e0) * 1000 ))
                if [ "$serving_ms" -le 5000 ]; then
                    line PASS serving-budget "start->serving ${serving_ms} ms ≤ 5000"
                else
                    line FAIL serving-budget "start->serving ${serving_ms} ms > 5000"
                fi
            else
                line UNCHECKED serving-budget "stamp has no serving pair to measure"
            fi
        else
            line UNCHECKED serving-budget "no daemon.build stamp"
        fi

        # 9. Policy decisions: the file absent means none can be made.
        if [ ! -f "$root/policy.json" ]; then
            line PASS policy "no policy.json — 0 decisions possible"
        elif [ -z "$usage_json" ]; then
            line UNCHECKED policy "policy.json present but usage --json gave no rows"
        else
            denials=$(echo "$usage_json" | awk -v s="$since_day" '
                match($0, /"day":"[^"]*"/) { day = substr($0, RSTART + 7, RLENGTH - 8) }
                match($0, /"tool_denials":[0-9]+/) {
                    n = substr($0, RSTART + 15, RLENGTH - 15) + 0
                    if (day >= s) total += n
                }
                END { print total + 0 }')
            if [ "$denials" -eq 0 ]; then
                line PASS policy "0 tool_denied record(s) since $since_day"
            else
                line UNCHECKED policy "$denials tool_denied record(s) — policy vs vendor needs the operator"
            fi
        fi

        # 10. Sync POSTs: never joined means nothing was ever sent.
        if [ -f "$root/sync.json" ]; then
            line UNCHECKED sync "$root/sync.json exists — this machine joined; POSTs need counting"
        else
            line PASS sync "no sync.json — never joined, 0 POSTs"
        fi

        line UNCHECKED operator-feel "only the operator can answer this one"

        # The window is open for CONBUS_CANARY_WINDOW_DAYS (default 7).
        # A fail inside it still exits 1 — 3 only means "not done yet".
        swapped_epoch=$(ts_epoch "$since")
        if [ "$fails" -gt 0 ]; then
            say "verdict: FAIL ($fails failing item(s))"
            exit 1
        elif is_uint "${swapped_epoch:-}" &&
            [ "$swapped_epoch" -gt 0 ] &&
            [ $((($(date +%s) - swapped_epoch) / 86400)) -lt "$window_days" ]; then
            say "verdict: window incomplete ($(( ($(date +%s) - swapped_epoch) / 86400 ))/${window_days} days) — not a pass yet"
            exit 3
        fi
        say "verdict: PASS"
        exit 0
        ;;

    restart)
        mode=$(swap_mode restart)
        [ -f "$state_file" ] ||
            die "no $state_file — a canary window starts with 'start'"
        old_stamp_pid=""
        if [ -f "$stamp" ]; then
            old_stamp_pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
        fi
        before=$(mktemp)
        drop_reasons >"$before"
        ts=$(date -u +%Y-%m-%dT%H:%M:%S.000Z)
        if [ "$mode" = rehearsal ]; then
            say "rehearsal: 'conbus install --apply' skipped (it would hit launchctl)"
            state_add_restart "$ts" -1 "$(new_drop_reasons "$before" | wc -l | tr -d ' ')"
            rm -f "$before"
            say "recorded a planned restart at $ts (rehearsal — no reload)"
            exit 0
        fi
        if ! "$bin" install --apply; then
            rm -f "$before"
            die "conbus install --apply failed — restart not recorded; check 'canary.sh status'"
        fi
        serving_ms=-1
        if wait_for_serving "$old_stamp_pid"; then
            started=$(sed -n 's/.*"started":"\([^"]*\)".*/\1/p' "$stamp")
            serving=$(sed -n 's/.*"serving":"\([^"]*\)".*/\1/p' "$stamp")
            e0=$(ts_epoch "$started") e1=$(ts_epoch "$serving")
            if is_uint "${e0:-}" && is_uint "${e1:-}"; then
                serving_ms=$(( (e1 - e0) * 1000 ))
            fi
        else
            say "restart: no new serving stamp inside ${serving_limit}s — check 'canary.sh status'"
        fi
        new_count=$(new_drop_reasons "$before" | wc -l | tr -d ' ')
        state_add_restart "$ts" "$serving_ms" "$new_count"
        rm -f "$before"
        say "recorded planned restart at $ts: serving ${serving_ms} ms, $new_count new drop reason(s)"
        ;;

    start)
        [ -n "$from" ] || refuse "start wants --from <candidate-binary>"
        [ -x "$from" ] || die "candidate is not an executable: $from"
        [ -x "$bin" ] || die "no installed conbus at $bin — canary only covers an install.sh install"
        mode=$(swap_mode start)
        if blackout_active; then
            if [ "$BLACKOUT_REASON" = unparseable ]; then
                refuse "start: CONBUS_CANARY_BLACKOUT='${CONBUS_CANARY_BLACKOUT:-}' is unparseable — treated as active, not swapping"
            fi
            refuse "start: CONBUS_CANARY_BLACKOUT is active (${CONBUS_CANARY_BLACKOUT:-}) — not swapping"
        fi
        cand_version=$("$from" --version 2>&1 || true)
        [ -n "$cand_version" ] || die "candidate does not run: $from"
        old_version=$("$bin" --version 2>&1 || echo unknown)
        cand_build=$(echo "$cand_version" | sed -n 's/.*(\([0-9a-f]*\)).*/\1/p')
        prev_build=$(echo "$old_version" | sed -n 's/.*(\([0-9a-f]*\)).*/\1/p')
        cand_sha=$(shasum -a 256 "$from" | awk '{print $1}')

        # preflight is part of start — a swap only ever happens over a
        # checked install. It refuses before anything moves.
        say "preflight:"
        if ! do_preflight; then
            refuse "start: preflight failed — not swapping"
        fi

        old_stamp_pid=""
        if [ -f "$stamp" ]; then
            old_stamp_pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
        fi
        before_drops=$(mktemp)
        drop_reasons >"$before_drops"

        # Stage the candidate beside the target first, then swap names.
        cp "$from" "$new_bin"
        chmod 755 "$new_bin"
        if [ ! -f "$rollback_bin" ]; then
            cp -p "$bin" "$rollback_bin"
            say "kept the current binary at $rollback_bin"
        fi
        mv -f "$new_bin" "$bin"
        say "swapped $bin: $old_version -> $cand_version ($mode)"

        swapped_ts=$(date -u +%Y-%m-%dT%H:%M:%S.000Z)
        state_write "$swapped_ts" "$prev_build" "$cand_build" "$cand_sha" '[]'

        # Automatic rollback on any post-swap failure — no `|| true` here.
        # The checks are the same in real and rehearsal: only the reload
        # (`install --apply`) is skipped in rehearsal, and the fakes answer
        # for the stamp and doctor there.
        fail_reason=""
        if ! reregister_daemon "$mode" && [ "$mode" = real ]; then
            fail_reason="conbus install --apply failed"
        elif ! wait_for_serving "$old_stamp_pid"; then
            fail_reason="no serving stamp from the new daemon within ${serving_limit}s"
        fi

        if [ -z "$fail_reason" ]; then
            doctor_out=$("$bin" doctor 2>&1 || true)
            if echo "$doctor_out" | grep -q 'DIFFERS'; then
                fail_reason="the answering daemon is a different build than $cand_build"
            elif ! echo "$doctor_out" | grep -q 'listening'; then
                fail_reason="the daemon is not answering after the swap"
            elif echo "$doctor_out" | grep '^hooks' | grep -qv ' runs '; then
                bad_hooks=$(echo "$doctor_out" | grep '^hooks' | grep -v ' runs ' | head -1)
                fail_reason="a hook is not 'runs': ${bad_hooks#hooks     }"
            fi
        fi

        if [ -z "$fail_reason" ]; then
            new_drops=$(new_drop_reasons "$before_drops")
            if [ -n "$new_drops" ]; then
                fail_reason="new drop reason(s) since the swap: $(echo "$new_drops" | tr '\n' ' ')"
            fi
        fi
        rm -f "$before_drops"

        if [ -n "$fail_reason" ]; then
            say "start: $fail_reason — rolling back"
            mv -f "$rollback_bin" "$bin"
            if reregister_daemon "$mode"; then
                wait_for_serving "$old_stamp_pid" || true
            fi
            state_write "$swapped_ts" "$prev_build" "$cand_build" "$cand_sha" '[]' \
                "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)"
            say "rolled back $bin to $("$bin" --version 2>&1 || echo unknown)"
            die "start failed: $fail_reason (rolled back automatically)"
        fi

        say "start: serving, matching build, hooks runs, no new drops — canary is live"
        ;;

    rollback)
        mode=$(swap_mode rollback)
        [ -f "$rollback_bin" ] ||
            die "no $rollback_bin — nothing to roll back to; start keeps one before the first swap"
        old_stamp_pid=""
        if [ -f "$stamp" ]; then
            old_stamp_pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
        fi
        mv -f "$rollback_bin" "$bin"
        say "rolled back $bin to $("$bin" --version 2>&1 || echo unknown) ($mode)"

        reload=0
        if reregister_daemon "$mode"; then reload=1; fi
        if [ "$reload" = 1 ]; then wait_for_serving "$old_stamp_pid" || true; fi
        check_doctor "$mode"
        ;;

    "" | -h | --help | help)
        sed -n '3,10p' "$0"
        ;;
    *)
        refuse "unknown subcommand: $sub (preflight|status|verdict|start|restart|rollback)"
        ;;
esac
