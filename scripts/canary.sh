#!/bin/bash
# Guarded canary control for this user's installed conbus.
#
#   scripts/canary.sh preflight [--evidence DIR]
#   scripts/canary.sh status
#   scripts/canary.sh verdict [--since <ts>]
#   scripts/canary.sh start --from <candidate-binary>
#   scripts/canary.sh rollback
#
# preflight, status and verdict are read-only (preflight also makes the
# plist backup — a copy, never a change). start and rollback swap the
# installed binary. Per the 1.0 plan, §7:
#   - the swap is real only under the real HOME with CONBUS_SKIP_LAUNCHCTL
#     unset; it then ends in `conbus install --apply` (bootout + bootstrap,
#     never kickstart — the stamp-poll and doctor still run after it);
#   - anything else is a rehearsal and needs CONBUS_CANARY_REHEARSAL=1
#     (--live is a convenience alias): the file swap still runs, launchctl
#     and `install --apply` are skipped and the script says so;
#   - a start during CONBUS_CANARY_BLACKOUT is refused. The spec is
#     HH:MM-HH:MM (daily, wrap allowed) or <epoch>-<epoch>; a spec that
#     parses as neither counts as an active blackout — fail safe.
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

is_uint() { case $1 in '' | *[!0-9]*) return 1 ;; *) return 0 ;; esac }

# The real environment: HOME is this user's passwd home and nothing asked
# that launchctl be skipped. Anything else must declare itself a rehearsal.
is_real_env() {
    [ -z "${CONBUS_SKIP_LAUNCHCTL:-}" ] || return 1
    real_home=$(env -u HOME bash -c 'cd && pwd -P' 2>/dev/null) || return 1
    [ "$(cd "$HOME" && pwd -P)" = "$real_home" ]
}

# Is a blackout window active right now? 0 = active, 1 = not. An unset spec
# is never active; a spec that parses as neither form counts as active —
# an unreadable window must not let a swap through.
blackout_active() {
    spec=${CONBUS_CANARY_BLACKOUT:-}
    [ -z "$spec" ] && return 1
    case $spec in
        [0-9][0-9]:[0-9][0-9]-[0-9][0-9]:[0-9][0-9])
            s=${spec%%-*}
            e=${spec##*-}
            sh=${s%%:*}
            sm=${s##*:}
            eh=${e%%:*}
            em=${e##*:}
            is_uint "$sh" && is_uint "$sm" && is_uint "$eh" && is_uint "$em" || return 0
            [ "$sh" -le 23 ] && [ "$sm" -le 59 ] && [ "$eh" -le 23 ] && [ "$em" -le 59 ] ||
                return 0
            now=$(date +%H:%M)
            if [ "$s" = "$e" ]; then
                return 0
            elif [ "$s" \< "$e" ]; then
                [ ! "$now" \< "$s" ] && [ "$now" \< "$e" ]
            else
                [ ! "$now" \< "$s" ] || [ "$now" \< "$e" ]
            fi
            ;;
        *-*)
            a=${spec%%-*}
            b=${spec##*-}
            is_uint "$a" && is_uint "$b" || return 0
            [ "$a" -lt "$b" ] || return 0
            now=$(date +%s)
            [ "$now" -ge "$a" ] && [ "$now" -lt "$b" ]
            ;;
        *)
            return 0
            ;;
    esac
}

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
        fail=0
        check() { # check <level: 0=PASS 1=FAIL 2=WARN> <name> <detail>
            case $1 in
                0) printf 'PASS %s %s\n' "$2" "$3" ;;
                2) printf 'WARN %s %s\n' "$2" "$3" ;;
                *)
                    printf 'FAIL %s %s\n' "$2" "$3"
                    fail=1
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
            if [ "$(uname -s)" = Darwin ]; then
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
            check 1 blackout "CONBUS_CANARY_BLACKOUT is active (${CONBUS_CANARY_BLACKOUT:-})"
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
        exit "$fail"
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
        ;;

    verdict)
        # The §7 pass criteria a machine can score, against the log and
        # drops since the swap. The boundary is the running daemon's
        # `started` stamp unless --since overrides it; fixed-width UTC
        # stamps compare as strings.
        fails=0
        line() { # line <PASS|FAIL|UNCHECKED> <name> <detail>
            printf '%s %s — %s\n' "$1" "$2" "$3"
            if [ "$1" = FAIL ]; then fails=$((fails + 1)); fi
        }

        if [ -z "$since" ] && [ -f "$stamp" ]; then
            since=$(sed -n 's/.*"started":"\([^"]*\)".*/\1/p' "$stamp")
        fi
        [ -n "$since" ] ||
            die "no --since and no daemon.build stamp — nothing to bound the window with"
        case $since in
            [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) since="$since"T00:00:00.000Z ;;
        esac
        since_day=${since:0:10}
        say "window: since $since"

        [ -x "$bin" ] || die "no installed conbus at $bin — verdict needs its reads"

        # 1. Unplanned recorder restarts: daemon start lines after since,
        #    minus the running daemon's own (newest) boot.
        starts=0
        if [ -f "$daemon_log" ]; then
            starts=$(awk -v s="$since" '
                $1 > s && /conbus daemon: .*: socket .* log / { n++ }
                /conbus daemon: .*: socket .* log / { last = $1 }
                END { if (last > s) n--; print n + 0 }
            ' "$daemon_log")
        else
            line UNCHECKED restarts "no $daemon_log"
        fi
        if [ -f "$daemon_log" ]; then
            if [ "$starts" -eq 0 ]; then
                line PASS restarts "0 unplanned daemon start(s) since $since"
            else
                line FAIL restarts "$starts unplanned daemon start(s) since $since"
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
        #    not new).
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
            if [ -z "$new_reasons" ]; then
                line PASS drops "0 new drop reason(s) ($drop_count drop(s) since)"
            else
                line FAIL drops \
                    "new drop reason(s) since $since: $(echo "$new_reasons" | tr '\n' ' ')"
            fi
        else
            line PASS drops "no $drops_log — nothing dropped"
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

        # 6. Policy decisions: the file absent means none can be made.
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

        # 7. Sync POSTs: never joined means nothing was ever sent.
        if [ -f "$root/sync.json" ]; then
            line UNCHECKED sync "$root/sync.json exists — this machine joined; POSTs need counting"
        else
            line PASS sync "no sync.json — never joined, 0 POSTs"
        fi

        line UNCHECKED usage-parity "needs the 0.2.8 binary — shadow-replay.sh covers it"
        line UNCHECKED rss-budget "needs an RSS/time measurement — bench-corpus.sh covers it"
        line UNCHECKED operator-feel "only the operator can answer this one"

        exit "$fails"
        ;;

    start)
        [ -n "$from" ] || refuse "start wants --from <candidate-binary>"
        [ -x "$from" ] || die "candidate is not an executable: $from"
        [ -x "$bin" ] || die "no installed conbus at $bin — canary only covers an install.sh install"
        mode=$(swap_mode start)
        if blackout_active; then
            refuse "start: CONBUS_CANARY_BLACKOUT is active (${CONBUS_CANARY_BLACKOUT:-}) — not swapping"
        fi
        cand_version=$("$from" --version 2>&1 || true)
        [ -n "$cand_version" ] || die "candidate does not run: $from"
        old_version=$("$bin" --version 2>&1 || echo unknown)
        old_stamp_pid=""
        if [ -f "$stamp" ]; then
            old_stamp_pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
        fi

        # Stage the candidate beside the target first, then swap names.
        cp "$from" "$new_bin"
        chmod 755 "$new_bin"
        if [ ! -f "$rollback_bin" ]; then
            cp -p "$bin" "$rollback_bin"
            say "kept the current binary at $rollback_bin"
        fi
        mv -f "$new_bin" "$bin"
        say "swapped $bin: $old_version -> $cand_version ($mode)"

        reload=0
        if reregister_daemon "$mode"; then reload=1; fi
        if [ "$reload" = 1 ]; then wait_for_serving "$old_stamp_pid" || true; fi
        check_doctor "$mode"
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
        sed -n '3,9p' "$0"
        ;;
    *)
        refuse "unknown subcommand: $sub (preflight|status|verdict|start|rollback)"
        ;;
esac
