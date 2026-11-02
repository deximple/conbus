#!/bin/bash
# Guarded canary control for this user's installed conbus.
#
#   scripts/canary.sh preflight [--evidence DIR]
#   scripts/canary.sh status
#   scripts/canary.sh verdict [--since <ts>] [--idle <harness|day>[,...]]
#   scripts/canary.sh start --from <candidate-binary>
#   scripts/canary.sh restart
#   scripts/canary.sh rollback
#
# preflight, status and verdict are read-only (preflight also makes the
# plist backup — a copy, never a change). start, restart and rollback swap
# or reload the installed binary. Per the 1.0 plan, §7:
#   - the swap is real only when ALL of these hold: HOME is the passwd
#     home, CONBUS_SKIP_LAUNCHCTL is unset, CONBUS_CANARY_REHEARSAL is
#     unset. It then ends in `conbus install --apply` (bootout +
#     bootstrap, never kickstart — the stamp-poll and doctor still run).
#   - a rehearsal needs CONBUS_CANARY_REHEARSAL=1 AND a throwaway HOME —
#     under the passwd home the flag is refused before anything moves.
#     The file swap still runs; launchctl and `install --apply` are
#     skipped and the script says so.
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
#     before — except the two a normal swap window produces (a protocol
#     refusal while the old daemon still answers, or no listener
#     mid-restart), which are counted into canary.json as
#     swap_window_drops instead. Any other failure rolls back
#     automatically and exits 1. The 5-second serving goal is a verdict
#     criterion, not a rollback one — current measurements sit above it
#     and would undo every cutover.
#   - state lives at <root>/canary.json: swap time, previous/candidate
#     builds, candidate sha, swap-window drop count, and the planned
#     restarts (each with its start->serving milliseconds and the number
#     of drop lines added during it — not new-reason strings).
#   - `restart` performs a planned restart (`install --apply`) and records
#     its measurements. `verdict` bounds its window by the recorded swap
#     time unless --since overrides it; its budgets come from
#     CONBUS_CANARY_TAIL_MS / _USAGE_MS / _DOCTOR_MS / _SERVING_MS /
#     _RSS_MB / _WINDOW_DAYS, defaulting to the plan's M3 targets.
#   - `verdict` exits 0 on a completed pass, 1 when an item fails, 2 when it
#     refuses its arguments, 3 while the 7-day window is still open, and 4
#     when the window is over but an item other than operator-feel could not
#     be judged. Exit 4 is neither a fail nor a pass; it names what is open.
#     --idle is the operator's answer to the one question a log cannot settle,
#     "was this harness or day idle, or not recorded?"; a PASS prints it.
#   - preflight fails on doctor's `disk` warning unless
#     CONBUS_CANARY_DISK_WARN_OK=1 downgrades it to WARN.
#   - every rollback — the automatic one after a failed start and the
#     `rollback` subcommand — is verified: the restored build must write a
#     daemon.build stamp with a new pid and doctor must listen with no
#     DIFFERS, else the script exits 3.
#   - every doctor call is capped at CONBUS_CANARY_DOCTOR_LIMIT seconds
#     (default 120): an old build's vendor probes are unbounded, and a
#     wedged one must fail the check it is in — not hang it. A cap hit in
#     verify_rollback exits 3 like any unverified rollback.
#
# The install dir is detected exactly the way install.sh places it:
# $HOME/.local/bin. Swaps write beside the target and rename — never a copy
# over a binary a running daemon has mapped.
set -euo pipefail

# Under pipefail a reader that leaves early (`grep -q`, `head`) can cost its writer
# a SIGPIPE, and the pipeline then reads as failed however the match came out: a
# healthy daemon as "not answering", a swap rolled back. So grep -q reads a
# here-string, and a first line comes from `sed -n '1p'`, which reads to the end.

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
# A build that rotates its drop log renames a full drops.jsonl here and
# starts fresh; the file sits beside the live one, never nested further.
drops_rotated=$root/drops.1.jsonl
# crates/conbus/src/drops.rs CAP_BYTES. A build that does not rotate (0.2.x,
# rc.3) stops recording the moment its log reaches this size, so a file at
# it is blind: every later drop was silently discarded and any "0 drops"
# verdict on it has no basis.
drops_cap_bytes=$((4 * 1024 * 1024))
state_file=$root/canary.json

serving_limit=${CONBUS_CANARY_SERVING_LIMIT:-30}
rss_budget_mb=${CONBUS_CANARY_RSS_MB:-256}
window_days=${CONBUS_CANARY_WINDOW_DAYS:-7}
# Verdict budgets — the plan's M3 targets, overridable per machine.
tail_ms_budget=${CONBUS_CANARY_TAIL_MS:-200}
usage_ms_budget=${CONBUS_CANARY_USAGE_MS:-2000}
doctor_ms_budget=${CONBUS_CANARY_DOCTOR_MS:-3000}
serving_ms_budget=${CONBUS_CANARY_SERVING_MS:-5000}
doctor_limit_s=${CONBUS_CANARY_DOCTOR_LIMIT:-120}

is_uint() { case $1 in '' | *[!0-9]*) return 1 ;; *) return 0 ;; esac }

# An ISO-8601 UTC timestamp with real milliseconds — `date` only writes a
# literal .000, and drop lines carry true ms, so a same-second boundary
# needs the fraction to order them.
now_iso_ms() {
    perl -MTime::HiRes=gettimeofday -MPOSIX=strftime -e \
        '($s, $u) = gettimeofday;
         printf "%s.%03dZ", strftime("%Y-%m-%dT%H:%M:%S", gmtime($s)), int($u / 1000)'
}

# doctor under a wall-clock cap — CONBUS_CANARY_DOCTOR_LIMIT seconds
# (default 120). 0.2.x's vendor probes have no bound and a wedged probe has
# hung this script outright. The call runs detached with its output in a
# file: a probe can outlive the doctor that spawned it, and a surviving
# probe holding the command-substitution pipe would hang the caller anyway.
# Exit 142 means the cap fired — callers treat it as "no answer", never as
# a verdict on the daemon's health.
run_doctor() {
    df=$(mktemp "${TMPDIR:-/tmp}/canary-doctor.XXXXXX") || return 1
    "$bin" doctor >"$df" 2>&1 &
    dpid=$!
    n=0
    # A short tick: time-budget times this call, and a long one reads as doctor.
    while kill -0 "$dpid" 2>/dev/null && [ $n -lt $((doctor_limit_s * 10)) ]; do
        sleep 0.1
        n=$((n + 1))
    done
    if kill -0 "$dpid" 2>/dev/null; then
        # A probe the doctor spawned is a direct child — name it before the
        # kill, since a dead doctor orphans it under launchd. Deeper
        # descendants still escape, same as any killed tree.
        kids=$(pgrep -P "$dpid" 2>/dev/null || true)
        kill $kids "$dpid" 2>/dev/null || true
        m=0
        while kill -0 "$dpid" 2>/dev/null && [ $m -lt 10 ]; do
            sleep 0.2
            m=$((m + 1))
        done
        cat "$df"
        rm -f "$df"
        return 142
    fi
    # The doctor's own status goes back to the caller: one that could not read
    # the log exits non-zero after printing most of its lines.
    doctor_status=0
    wait "$dpid" 2>/dev/null || doctor_status=$?
    cat "$df"
    rm -f "$df"
    return "$doctor_status"
}

# HOME resolves to this user's passwd home — independent of SKIP or
# REHEARSAL, so a rehearsal under it can be refused outright. `cd` with
# HOME unset fails in bash, but `cd ~` resolves the login entry via
# getpwuid — no dscl/getent/getpwnam dependency.
passwd_home() {
    env -u HOME bash -c 'cd ~ && pwd -P' 2>/dev/null
}

is_passwd_home() {
    local pw
    pw=$(passwd_home) || return 1
    [ -n "$pw" ] || return 1
    [ "$(cd "$HOME" && pwd -P)" = "$(cd "$pw" && pwd -P)" ]
}

# The real environment: HOME is this user's passwd home and nothing asked
# that launchctl be skipped. CONBUS_CANARY_TEST_REAL_HOME is the
# rehearsal's seam into the real code path: honoured only when it resolves
# to the same throwaway HOME the script sees, which by construction is not
# the passwd home. Naming the passwd home through it can never launder a
# real swap.
is_real_env() {
    [ -z "${CONBUS_SKIP_LAUNCHCTL:-}" ] || return 1
    is_passwd_home && return 0
    [ -n "${CONBUS_CANARY_TEST_REAL_HOME:-}" ] || return 1
    this_home=$(cd "$HOME" && pwd -P)
    [ "$(cd "$CONBUS_CANARY_TEST_REAL_HOME" 2>/dev/null && pwd -P)" = "$this_home" ]
}

# True when `agy models` lists models. With no login it fails and opens nothing.
agy_signed_in() {
    "${CONBUS_AGY_BIN:-agy}" models >/dev/null 2>&1 </dev/null &
    agy_pid=$!
    n=0
    while kill -0 "$agy_pid" 2>/dev/null && [ "$n" -lt 150 ]; do
        sleep 0.1
        n=$((n + 1))
    done
    if kill -0 "$agy_pid" 2>/dev/null; then
        kill "$agy_pid" 2>/dev/null || true
        wait "$agy_pid" 2>/dev/null || true
        return 1
    fi
    wait "$agy_pid" 2>/dev/null
}

# With no login, `agy -p /usage` opens a browser tab for OAuth, terminal or no
# terminal, and doctor on a build older than conbus's own sign-in check still
# runs it. That build may be the one installed or the one rolled back to, so
# the check is made here too. A throwaway HOME has no login to ask about, and a
# quota line is not what this script tests.
if ! is_real_env || ! agy_signed_in; then
    export CONBUS_AGY_BIN=/usr/bin/false
fi

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

# Decide real vs rehearsal for the swap subcommands. Rehearsal is checked
# first: CONBUS_CANARY_REHEARSAL under the passwd home is refused before
# anything moves — a rehearsal that ran the real path would be a half
# deploy. A real swap needs the real HOME, CONBUS_SKIP_LAUNCHCTL unset and
# REHEARSAL unset all at once. Everything else is refused.
swap_mode() {
    sub=$1
    if [ -n "${CONBUS_CANARY_REHEARSAL:-}" ]; then
        if is_passwd_home; then
            refuse "$sub: CONBUS_CANARY_REHEARSAL is set under the real HOME —" \
                "rehearsing must not touch the real install; refusing before anything moves"
        fi
        echo rehearsal
        return
    fi
    if is_real_env; then
        echo real
        return
    fi
    refuse "$sub: the real swap needs the real HOME with" \
        "CONBUS_SKIP_LAUNCHCTL unset; a rehearsal needs" \
        "CONBUS_CANARY_REHEARSAL=1 under a throwaway HOME"
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

# Wait for the stamp to name <build> with a pid that is not <not_pid> — the
# proof a rollback actually booted. A restored 0.2.x stamp carries no
# `serving` key, so unlike wait_for_serving this never asks for one.
wait_for_build() {
    want_build=$1
    not_pid=$2
    n=0
    limit=$((serving_limit * 2))
    while [ $n -lt "$limit" ]; do
        if [ -f "$stamp" ]; then
            pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
            b=$(sed -n 's/.*"build":"\([^"]*\)".*/\1/p' "$stamp")
            if [ "$b" = "$want_build" ] && [ -n "$pid" ] && [ "$pid" != "$not_pid" ]; then
                echo "$pid"
                return 0
            fi
        fi
        sleep 0.5
        n=$((n + 1))
    done
    return 1
}

# After a rollback the old build must come back: a stamp naming it with a
# pid the candidate never had, then a doctor that is listening and agrees.
# Verified prints and returns; anything else exits 3 — a rollback nobody
# verified is worse news than the failure that caused it.
verify_rollback() {
    prev_build=$1
    cand_pid=$2
    mode=$3
    if [ "$mode" = rehearsal ]; then
        # The reload is skipped under rehearsal; let the fake boot the old
        # build's daemon the way install --apply would have.
        run_doctor >/dev/null 2>&1 || true
    fi
    rb_pid=$(wait_for_build "$prev_build" "$cand_pid") || {
        say "ROLLBACK NOT VERIFIED — no $prev_build stamp inside ${serving_limit}s; run 'canary.sh status'"
        exit 3
    }
    doctor_rc=0
    doctor_out=$(run_doctor) || doctor_rc=$?
    if [ "$doctor_rc" -eq 142 ]; then
        say "ROLLBACK NOT VERIFIED — doctor did not answer inside ${doctor_limit_s}s; run 'canary.sh status'"
        exit 3
    fi
    if grep -q 'DIFFERS' <<<"$doctor_out"; then
        say "ROLLBACK NOT VERIFIED — doctor reports a different build; run 'canary.sh status'"
        exit 3
    fi
    if ! grep -q 'listening' <<<"$doctor_out"; then
        say "ROLLBACK NOT VERIFIED — the daemon is not listening; run 'canary.sh status'"
        exit 3
    fi
    say "rollback verified: $prev_build pid $rb_pid listening"
}

# Report the socket state and fail (1) only when a real reload did not come
# up; in rehearsal, not-listening is expected.
check_doctor() {
    mode=$1
    doctor_rc=0
    doctor_out=$(run_doctor) || doctor_rc=$?
    [ "$doctor_rc" -eq 142 ] &&
        say "doctor: no answer inside ${doctor_limit_s}s (cap)"
    echo "$doctor_out" | grep -E '^(socket|daemon|recorder|hardening)' || true
    if grep -q 'DIFFERS' <<<"$doctor_out"; then
        say "doctor: the answering daemon is a different build —"
        say "  check \`canary.sh status\`, or roll back: canary.sh rollback"
        return 1
    fi
    if grep -q 'listening' <<<"$doctor_out"; then
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
    sed -n "s/.*\"$1\":\"\\([^\"]*\\)\".*/\\1/p" "$state_file" | sed -n '1p'
}

state_restart_drop_total() {
    [ -f "$state_file" ] || { echo 0; return; }
    { grep -o '"new_drops":[0-9]*' "$state_file" || true; } | cut -d: -f2 |
        awk '{ t += $1 } END { print t + 0 }'
}

state_write() { # state_write <swapped> <prev> <cand> <sha> <restarts-json-array> [rolled_back] [swap_window_drops]
    mkdir -p "$root"
    tmp=$state_file.tmp
    printf '{"swapped":"%s","prev_build":"%s","candidate_build":"%s","candidate_sha256":"%s","restarts":%s%s%s}\n' \
        "$1" "$2" "$3" "$4" "$5" "${6:+,\"rolled_back\":\"$6\"}" \
        "${7:+,\"swap_window_drops\":$7}" >"$tmp"
    mv -f "$tmp" "$state_file"
}

state_restarts_json() { # echoes the restarts array as stored, [] when absent
    [ -f "$state_file" ] || { echo '[]'; return; }
    sed -n 's/.*"restarts":\(\[.*\]\).*/\1/p' "$state_file" | sed -n '1p'
}

state_swap_window_drops() {
    sed -n 's/.*"swap_window_drops":\([0-9]*\).*/\1/p' "$state_file"
}

state_add_restart() { # state_add_restart <ts> <serving_ms|-> <new_drops>
    restarts=$(state_restarts_json)
    entry=$(printf '{"ts":"%s","serving_ms":%s,"new_drops":%s}' "$1" "${2:--1}" "${3:-0}")
    if [ "$restarts" = '[]' ]; then
        new="[$entry]"
    else
        new="${restarts%\]},$entry]"
    fi
    # All of it: dropping rolled_back here would turn a rolled-back window back
    # into a live one the moment anyone restarted.
    state_write "$(state_get swapped)" "$(state_get prev_build)" \
        "$(state_get candidate_build)" "$(state_get candidate_sha256)" "$new" \
        "$(state_get rolled_back)" "$(state_swap_window_drops)"
}

# Spool files left after a rollback — the restored 0.2.x build does not
# read a spool, so they sit until a newer daemon drains them. Reported,
# never deleted here.
say_spool() {
    n=0
    [ -d "$root/spool" ] && n=$(ls "$root/spool" 2>/dev/null | wc -l | tr -d ' ')
    say "spool: $n file(s) — the old build does not drain them; they wait for the next daemon"
}

# What a drop line means. The classes come from what the hook and the daemon
# write (crates/conbus/src/hook/client.rs, daemon.rs, drops.rs) and from what
# this log's real history shows each of them to be:
#   lost        a record that did not land. A build that spools only writes these
#               when the spool refused too. Fails at any count, known or not.
#               Includes an orphaned tool or prompt event: real content with no
#               conversation to go in.
#   gap         the adapter has no mapping for an event (`no mapping`, `no
#               adapter`): the vendor moved. Fails at any count, named by event.
#   ghost       the end of a session or a turn, or a notification, for a session
#               with no binding. It carries no content of its own: 693 of the 729
#               orphan lines in the three weeks before the swap, 8 of them within two
#               minutes of a daemon start, and 350 of the 405 sessions behind those
#               729 never reached the log.
#   sessionless the payload named no session. The plan counts this as the vendor's,
#               not a lost record.
#   payload     the hook could not parse what it was given; it cannot say whether
#               that was a record. 64 lines in eight weeks.
#   held        `the log refused writes for a while` (#155): an outage's one line,
#               written once a write worked, when every record waited and landed.
#               The same line with `and records may be lost` is lost.
#   elided      `tool bodies elided, not sealed` (#158): the record landed, its
#               tool body did not. Judged on its own, as a share of tool calls.
#   other       none of the above: new since the swap fails, known needs a person.
drop_awk_lib='
function drop_parse(line) {
    d_ts = ""; d_reason = ""; d_detail = ""
    if (match(line, /"ts":"[^"]*"/)) d_ts = substr(line, RSTART + 6, RLENGTH - 7)
    if (match(line, /"reason":"([^"\\]|\\.)*"/)) d_reason = substr(line, RSTART + 10, RLENGTH - 11)
    if (match(line, /"detail":"([^"\\]|\\.)*"/)) d_detail = substr(line, RSTART + 10, RLENGTH - 11)
}
function drop_event(detail,    event) {
    event = detail
    sub(/ for session .*/, "", event)
    sub(/ as .*/, "", event)
    return event
}
function starts(text, prefix) { return index(text, prefix) == 1 }
function drop_class(r, d) {
    if (r == "tool bodies elided, not sealed") return "elided"
    if (r == "the log refused writes for a while") return "held"
    if (starts(r, "the log refused writes for a while")) return "lost"
    if (starts(r, "no mapping for hook event") || starts(r, "no adapter for harness")) return "gap"
    if (index(r, "names no session")) return "sessionless"
    if (starts(r, "no conversation for this session") || starts(r, "spooled hook still has no conversation")) {
        if (drop_event(d) ~ /^([Ss]ession_?[Ee]nd(ed)?|([Ss]ubagent)?[Ss]top|turn_ended|[Nn]otification|notified)$/)
            return "ghost"
        return "lost"
    }
    if (starts(r, "no daemon on ") || starts(r, "could not send the record:") ||
        starts(r, "no reply from the daemon:") || starts(r, "unreadable reply ") ||
        starts(r, "could not bound the daemon call:") || starts(r, "the daemon refused the record:") ||
        starts(r, "spooled hook refused") || starts(r, "spool entry ") ||
        starts(r, "no home directory to resolve") || starts(r, "the socket path "))
        return "lost"
    if (starts(r, "could not read the hook payload") || r ~ / at line [0-9]+ column [0-9]+$/) return "payload"
    return "other"
}
# The two reasons a normal swap window produces. Between the mv and the new
# daemon serving, a firing hook either reaches the old daemon, which refuses a
# protocol it does not speak and cannot be spooled past (an old stamp names no
# retention), or finds nobody listening. A same-version refusal is not one.
function drop_swap_reason(r,    old_v, new_v) {
    if (starts(r, "no daemon on ")) return index(r, ": Connection refused") > 0
    if (!match(r, /^the daemon refused the record: this daemon speaks protocol v[0-9]+; the hook speaks v[0-9]+/)) return 0
    old_v = r; sub(/^.*speaks protocol v/, "", old_v); sub(/;.*/, "", old_v)
    new_v = r; sub(/^.*the hook speaks v/, "", new_v); sub(/[^0-9].*/, "", new_v)
    return old_v != new_v
}
function drop_gap_name(r,    rest, event, as) {
    rest = r
    sub(/^[^"]*"/, "", rest)
    event = rest
    sub(/\\".*/, "", event)
    if (starts(r, "no adapter")) return "harness " event
    as = ""
    if (match(rest, /\(as [a-z_]+/)) as = " as " substr(rest, RSTART + 4, RLENGTH - 4)
    return event as
}
'

# The distinct drop reasons across the live file and its rotated generation — a
# flood that rotated the log mid-window put its reason in drops.1, and reading
# drops.jsonl alone would miss it. Whole reasons, escaped quotes and all: stopping
# at the first \" made every `no mapping for hook event \"X\"` one reason.
drop_reasons() {
    for f in "$drops_rotated" "$drops_log"; do
        # `continue`, not `&&`: a missing last file would end the loop non-zero
        # and take the whole script down under pipefail, without a word.
        [ -r "$f" ] || continue
        LC_ALL=C awk "$drop_awk_lib"'{ drop_parse($0); if (d_reason != "") print d_reason }' "$f"
    done | LC_ALL=C sort -u
}

# "<class><tab><reason>" for each drop line later than <ts>, from both
# generations: a rotation can rename the file mid-window, so "after this moment"
# is a timestamp cut, not a line count. `swap` marks a reason a swap window makes.
drop_classes_after() { # drop_classes_after <ts>
    dca_after=$1
    shift
    if [ -r "$drops_rotated" ]; then set -- "$@" "$drops_rotated"; fi
    if [ -r "$drops_log" ]; then set -- "$@" "$drops_log"; fi
    [ $# -gt 0 ] || return 0
    LC_ALL=C awk -v s="$dca_after" "$drop_awk_lib"'
        {
            drop_parse($0)
            if (d_ts > s && d_reason != "")
                printf "%s\t%s\n", (drop_swap_reason(d_reason) ? "swap" : drop_class(d_reason, d_detail)), d_reason
        }' "$@"
}

# True when this binary renames a full drop log aside. It names that file, and a
# build that does not rotate has no reason to.
build_rotates() { # build_rotates <binary>
    LC_ALL=C grep -aq 'drops\.1\.jsonl' "$1" 2>/dev/null
}

# True when drops.jsonl reached the cap under a build that does not rotate: it
# stopped recording, and nothing after that point is observable. On a build that
# rotates, a file at the cap only means the next drop renames it.
drops_blind() { # drops_blind <binary that will be writing>
    [ -r "$drops_log" ] || return 1
    [ "$(wc -c <"$drops_log" | tr -d ' ')" -ge "$drops_cap_bytes" ] || return 1
    ! build_rotates "$1"
}

# Reasons in the second file/list that the first did not have.
# new_drop_reasons <before-file> — prints nothing when there is no diff.
new_drop_reasons() {
    tmp=$(mktemp)
    drop_reasons >"$tmp"
    comm -13 "$1" "$tmp"
    rm -f "$tmp"
}

# The value of `<name> <value>` in rows an item built.
row_value() { # row_value <rows> <name>
    printf '%s\n' "$1" | sed -n "s/^$2 //p"
}

# One fact per line for verdict to judge: `<name> <number>`, and `item <class> <n>
# <what>` for what a person needs to read. Every comparison is on the text in C
# locale, so one invalid byte cannot lose a line.
drop_table() { # drop_table <since> <end of the swap window> <file>...
    dt_since=$1 dt_end=$2
    shift 2
    LC_ALL=C awk -v s="$dt_since" -v swap_end="$dt_end" -v rotated="$drops_rotated" "$drop_awk_lib"'
        {
            drop_parse($0)
            if (FILENAME == rotated && FNR == 1 && d_ts != "") first_rotated = d_ts
            if (d_ts == "" || d_reason == "") { bad++; next }
            if (d_ts > last) last = d_ts
            if (!(d_ts > s)) {
                if (!(d_reason in before)) { before[d_reason] = 1; baseline++ }
                next
            }
            lines++
            c = drop_class(d_reason, d_detail)
            if (c == "lost" && drop_swap_reason(d_reason) && !(d_ts > swap_end)) { swap++; next }
            if (c == "elided") {
                if (d_detail ~ /^[0-9]+ tool event\(s\)$/) elided += d_detail + 0
                else elided_bad++
                next
            }
            if (c == "gap") key = drop_gap_name(d_reason)
            else if (c == "ghost" || c == "sessionless" || c == "held") { key = d_detail; sub(/ \(fields:.*/, "", key); sub(/ for session .*/, "", key) }
            else if (c == "lost" && index(d_detail, " for session ")) key = drop_event(d_detail) ": " d_reason
            else key = d_reason
            if (key == "") key = "(no detail)"
            if (c == "other") {
                other_key[key] = 1
                n_other[key]++
                next
            }
            total[c]++
            n[c SUBSEP key]++
        }
        END {
            for (key in other_key) {
                c = (key in before) ? "known" : "new"
                total[c] += n_other[key]
                n[c SUBSEP key] = n_other[key]
            }
            printf "lines %d\nswap %d\nbad %d\nlast %s\nfirst_rotated %s\nelided %d\nelided_bad %d\nbaseline %d\n",
                lines, swap, bad, last, first_rotated, elided, elided_bad, baseline
            for (c in total) printf "total_%s %d\n", c, total[c]
            for (entry in n) {
                split(entry, part, SUBSEP)
                printf "item %s %d× %s\n", part[1], n[entry], substr(part[2], 1, 150)
            }
        }' "$@"
}

# 3. Drops, judged by what each line means. Whether a reason's text is new says
# nothing about whether a record was lost, and a week of no-daemon lines used to
# pass because the words had been seen before.
verdict_drops() {
    dt_rows=""
    dt_problem=""
    set --
    for f in "$drops_rotated" "$drops_log"; do
        [ -f "$f" ] || continue
        if [ -r "$f" ]; then
            set -- "$@" "$f"
        else
            dt_problem="$dt_problem $f"
        fi
    done
    dt_end=$(perl -MPOSIX=strftime -e 'print strftime("%Y-%m-%dT%H:%M:%S.000Z", gmtime($ARGV[0] + 60))' \
        "$(ts_epoch "${recorded_swap:-$since}")")
    if [ $# -gt 0 ]; then
        dt_rc=0
        dt_rows=$(drop_table "$since" "$dt_end" "$@") || dt_rc=$?
        [ "$dt_rc" -eq 0 ] || dt_problem="$dt_problem (reading them exited $dt_rc)"
    fi
    restart_drops=$(state_restart_drop_total)
    n_lost=$(row_value "$dt_rows" total_lost)
    n_gap=$(row_value "$dt_rows" total_gap)
    n_new=$(row_value "$dt_rows" total_new)
    n_known=$(row_value "$dt_rows" total_known)
    n_ghost=$(row_value "$dt_rows" total_ghost)
    n_sessionless=$(row_value "$dt_rows" total_sessionless)
    n_payload=$(row_value "$dt_rows" total_payload)
    n_held=$(row_value "$dt_rows" total_held)
    n_bad=$(row_value "$dt_rows" bad)
    n_lines=$(row_value "$dt_rows" lines)
    first_rotated=$(row_value "$dt_rows" first_rotated)
    last_drop=$(row_value "$dt_rows" last)
    elided_total=$(row_value "$dt_rows" elided)
    elided_bad=$(row_value "$dt_rows" elided_bad)
    dt_baseline=$(row_value "$dt_rows" baseline)
    swap_exempted=$(row_value "$dt_rows" swap)
    swap_exempted=${swap_exempted:-0}

    # What cannot be read leaves the item open; what can be read still counts. A
    # log that went blind on day 5 still shows what days 1 to 4 lost.
    open=""
    [ -z "$dt_problem" ] || open="$open; cannot read:$dt_problem"
    if [ "${n_bad:-0}" -gt 0 ]; then
        open="$open; ${n_bad} line(s) have no readable time or reason"
    fi
    if drops_blind "$bin"; then
        open="$open; $drops_log reached the cap on a build that does not rotate: nothing after ${last_drop:-its last line} was recorded"
    fi
    # The rotated generation keeps the file as it was, so its first line predates
    # the swap unless a second rotation threw that part away.
    discarded=0
    if [ -n "$first_rotated" ] && [ "$first_rotated" \> "$since" ]; then
        discarded=1
        open="$open; the oldest drop line left is at $first_rotated, after the swap: anything dropped before it was rotated away"
    fi
    if [ "${n_known:-0}" -gt 0 ]; then
        open="$open; ${n_known} line(s) with a known reason no rule covers need a person to say what they mean"
    fi
    for noisy in "ghost:$n_ghost" "sessionless:$n_sessionless" "payload:$n_payload"; do
        if [ "${noisy#*:}" != "" ] && [ "${noisy#*:}" -gt "$noise_limit" ]; then
            open="$open; ${noisy#*:} ${noisy%%:*} line(s) are over the limit of $noise_limit (CONBUS_CANARY_NOISE_MAX): too many to call background"
        fi
    done

    # A reason never seen needs a baseline to call it new. With the baseline
    # rotated away it is a question, not a fault.
    n_unseen=${n_new:-0}
    if [ "$n_unseen" -gt 0 ] && [ "${dt_baseline:-0}" -eq 0 ] && [ "$discarded" = 1 ]; then
        open="$open; $n_unseen line(s) with a reason no rule covers, and the baseline that would say whether it is new was rotated away"
        n_unseen=0
    fi

    faults=""
    [ "${n_lost:-0}" -eq 0 ] || faults="$faults, ${n_lost} lost record(s)"
    [ "${n_gap:-0}" -eq 0 ] || faults="$faults, ${n_gap} adapter gap(s)"
    [ "$n_unseen" -eq 0 ] || faults="$faults, $n_unseen line(s) with a reason not seen before the swap"
    [ "$restart_drops" -eq 0 ] || faults="$faults, $restart_drops drop(s) during planned restarts"

    shown=$((${n_ghost:-0} + ${n_sessionless:-0} + ${n_payload:-0} + ${n_held:-0}))
    tally="${n_lines:-0} line(s) since $since: $swap_exempted expected swap-window, $shown not lost, $restart_drops on planned restarts"
    if [ -n "$faults" ]; then
        line FAIL drops "${faults#, } ($tally)${open:+$open}"
    elif [ -n "$open" ]; then
        line UNCHECKED drops "${open#; } ($tally)"
    else
        line PASS drops "0 lost records ($tally)"
    fi

    # What was decided, so a person can argue with it.
    group() { # group <class> <heading>
        group_rows=$(row_value "$dt_rows" "item $1" | sort)
        [ -n "$group_rows" ] || return 0
        printf '  %s\n' "$2"
        printf '%s\n' "$group_rows" | sed 's/^/    /'
    }
    group lost "lost records — fail at any count, known reason or not:"
    group gap "adapter gaps — fail at any count; the vendor moved ahead of this build:"
    group new "reasons not seen before the swap — fail:"
    group known "known reasons no rule covers — not a pass until a person reads them:"
    group ghost "end of a session or turn with no binding — not lost, shown only:"
    group sessionless "payload named no session — the vendor's, not lost, shown only:"
    group payload "payload the hook could not parse — cannot prove a loss, shown only:"
    group held "the log refused writes and every record waited and landed — not lost, shown only:"
    printf '  rule: a lost record or adapter gap fails at any count; a reason new since the swap fails; the three shown kinds stop a pass only above %s lines each; the swap window exempts its two reasons for 60 s\n' "$noise_limit"
    if [ -r "$daemon_log" ]; then
        refused_hooks=$(awk -v s="$since" '$1 ~ /^[0-9]/ && $1 > s && /refusing a hook speaking protocol/ { n++ } END { print n + 0 }' "$daemon_log")
        if [ "$refused_hooks" -gt 0 ]; then
            printf '  note: daemon.log names %s refused hook(s) since the window began, and %s expected swap-window drop line(s) exist here; a refusal with no line was spooled, or its line never landed\n' "$refused_hooks" "$swap_exempted"
        fi
    fi
}

# 4. Activity. Only what the log holds can be read here, so the questions are:
# which days have no record at all, which harness used before the swap has none
# since, and how much of what is here the recorder saw itself. A usage row has no
# record total, only counts and `recovered`. Recovered turns carry no tool calls
# and two or more records each, so a row with tool calls, or more counted than
# recovered, holds something the recorder saw; on the real log every harness-day
# from before the recorder existed reads as recovered-only by this test.
verdict_activity() {
    if [ -n "$window_problem" ]; then
        line UNCHECKED harness-activity "$window_problem"
        return
    fi
    va_closed=""
    va_day=$since_day
    va_guard=0
    while [ "$va_day" \< "$today_utc" ] && [ "$va_guard" -lt 370 ]; do
        va_closed="$va_closed $va_day"
        va_day=$(next_day "$va_day") || break
        va_guard=$((va_guard + 1))
    done
    # The two weeks before the swap decide who counts as "active before it". A
    # harness last used in September is not news.
    va_lookback=$(perl -MTime::Local -MPOSIX=strftime -e '
        my ($y, $m, $d) = $ARGV[0] =~ /^(\d+)-(\d+)-(\d+)/;
        print strftime("%Y-%m-%d", gmtime(timegm(0, 0, 0, $d, $m - 1, $y) - 14 * 86400))' "$since_day")
    va_rows=$({
        printf '%s\n' "$usage_json" | sed 's/^/B /'
        printf '%s\n' "$window_json" | sed 's/^/W /'
    } | LC_ALL=C awk -v closed="$va_closed" -v lb="$va_lookback" -v sd="$since_day" '
        function num(key,    re) {
            re = "\"" key "\":[0-9]+"
            if (!match($0, re)) return 0
            return substr($0, RSTART + length(key) + 3, RLENGTH - length(key) - 3) + 0
        }
        function str(key,    re) {
            re = "\"" key "\":\"[^\"]*\""
            if (!match($0, re)) return ""
            return substr($0, RSTART + length(key) + 4, RLENGTH - length(key) - 5)
        }
        function saw_live(    counted, rec) {
            rec = num("recovered")
            counted = num("turns") + num("limit_stops") + num("account_stops") + num("vendor_errors") + \
                num("request_faults") + num("other_stops") + num("user_stops") + num("subagents") + \
                num("tool_calls") + num("tool_denials") + num("permission_asks") + num("briefings") + num("hops")
            return rec == 0 || num("tool_calls") > 0 || counted > rec
        }
        {
            d = str("day"); h = str("harness")
            if (d == "" || h == "") next
            if ($1 == "B") {
                if (d >= lb && d <= sd && saw_live()) {
                    active_before[h] = 1
                    if (d < sd && !((h, d) in before_day)) { before_day[h, d] = 1; before_days[h]++ }
                }
                next
            }
            any_row = 1
            day_seen[d] = 1
            harness_seen[h] = 1
            recovered[h] += num("recovered")
            turns[h] += num("turns")
            tools[h] += num("tool_calls")
            if (saw_live()) {
                day_live[d] = 1
                if (!((h, d) in live_day)) { live_day[h, d] = 1; live_days[h]++ }
            }
        }
        END {
            n = split(closed, days, " ")
            for (i = 1; i <= n; i++)
                if (!(days[i] in day_seen)) empty = empty " " days[i]
            for (d in day_seen)
                if (!(d in day_live)) recovered_days = recovered_days " " d
            for (h in active_before)
                if (!(h in harness_seen)) silent = silent " " h
            for (h in harness_seen) {
                if (live_days[h] == 0 && (h in active_before)) only_recovered = only_recovered " " h ":" recovered[h]
                if (before_days[h] == 14) {
                    missed = ""
                    for (i = 1; i <= n; i++)
                        if ((days[i] in day_seen) && !((h, days[i]) in live_day)) missed = missed "," days[i]
                    if (missed != "") streak = streak " " h ":" substr(missed, 2)
                }
                printf "table %s: the recorder saw it on %d of %d day(s) (%d turn(s), %d tool call(s)); gap fill recovered %d record(s)\n",
                    h, live_days[h], n + 1, turns[h], tools[h], recovered[h]
            }
            printf "any %d\nempty%s\nrecovered_days%s\nsilent%s\nonly_recovered%s\nstreak%s\n",
                any_row, empty, recovered_days, silent, only_recovered, streak
        }')

    if [ "$(row_value "$va_rows" any)" != 1 ]; then
        line UNCHECKED harness-activity \
            "no record since $since — nothing recorded to judge, and no answer from a person changes that"
        return
    fi
    va_faults="" va_open="" va_answered=""
    for pair in $(row_value "$va_rows" only_recovered); do
        va_faults="$va_faults; ${pair%%:*}: used since the swap, but only gap fill has it (${pair#*:} record(s)) — the recorder saw none of it"
    done
    va_list=$(row_value "$va_rows" recovered_days)
    if [ -n "$va_list" ]; then
        va_faults="$va_faults; only gap fill has these day(s), the recorder saw nothing on them: $va_list"
    fi
    va_ask() { # va_ask <name> <question>; the answer from --idle, or an open item
        if is_idle "$1"; then
            va_answered="$va_answered $1"
            operator_answers="$operator_answers $1"
        else
            va_open="$va_open; $2"
        fi
    }
    for va_item in $(row_value "$va_rows" empty); do
        va_ask "$va_item" "no record at all on $va_item — idle, or the recorder down?"
    done
    for va_item in $(row_value "$va_rows" silent); do
        va_ask "$va_item" "$va_item recorded in the two weeks before the swap and not since — not used, or not recorded?"
    done
    for pair in $(row_value "$va_rows" streak); do
        va_ask "${pair%%:*}" "${pair%%:*} recorded on all 14 days before the swap and not on ${pair#*:} — not used, or not recorded?"
    done

    va_answer=${va_answered:+; on the operator\'s word, idle:$va_answered}
    if [ -n "$va_faults" ]; then
        line FAIL harness-activity "${va_faults#; }${va_open}${va_answer}"
    elif [ -n "$va_open" ]; then
        line UNCHECKED harness-activity "${va_open#; } — answer with --idle <harness|day>, by name${va_answer}"
    else
        line PASS harness-activity "every closed day has records and every harness active before the swap has some since${va_answer}"
    fi
    printf '%s\n' "$va_rows" | sed -n 's/^table /  /p' | sort
}

# Is what ran this window still what the window judges? Read back from the files
# themselves: `start` stamps the candidate, and nothing after it checked.
verdict_build() {
    cand_build=$(state_get candidate_build || true)
    cand_sha=$(state_get candidate_sha256 || true)
    rolled=$(state_get rolled_back || true)
    if [ -z "$cand_build" ]; then
        line UNCHECKED build "canary.json names no candidate — nothing says which build this window judges"
        return
    fi
    vb_faults="" vb_open=""
    if [ -n "$rolled" ]; then
        vb_faults="$vb_faults; rolled back at $rolled — this window no longer judges the candidate"
    fi
    vb_installed=$("$bin" --version 2>&1 | sed -n 's/.*(\([0-9a-f]*\)).*/\1/p' || true)
    if [ "$vb_installed" != "$cand_build" ]; then
        vb_faults="$vb_faults; the installed binary is build ${vb_installed:-unknown}, the candidate is $cand_build"
    fi
    vb_sha=$(shasum -a 256 "$bin" 2>/dev/null | awk '{ print $1 }' || true)
    if [ -z "$cand_sha" ]; then
        vb_open="canary.json names no candidate sha256"
    elif [ "$vb_sha" != "$cand_sha" ]; then
        vb_faults="$vb_faults; the installed binary's sha256 is not the candidate's"
    fi
    vb_running=$(sed -n 's/.*"build":"\([^"]*\)".*/\1/p' "$stamp" 2>/dev/null || true)
    if [ "$vb_running" != "$cand_build" ]; then
        vb_faults="$vb_faults; the daemon's stamp names build ${vb_running:-none}, the candidate is $cand_build"
    fi
    if [ -n "$doctor_failed" ]; then
        vb_open="${vb_open:+$vb_open; }$doctor_failed"
    else
        vb_doctor=$(printf '%s\n' "$doctor_out" | grep -E 'DIFFERS|the stamp is stale|^recorder  starting' || true)
        if [ -n "$vb_doctor" ]; then
            vb_faults="$vb_faults; doctor says: $(printf '%s\n' "$vb_doctor" | sed -n '1p' | cut -c1-160)"
        fi
    fi
    if [ -n "$vb_faults" ]; then
        line FAIL build "${vb_faults#; }"
    elif [ -n "$vb_open" ]; then
        line UNCHECKED build "the binary and stamp name the candidate, and it could not be confirmed: $vb_open"
    else
        line PASS build "the installed binary and the running daemon are candidate $cand_build (sha256 agrees); never rolled back"
    fi
}

# The lines doctor prints that mean a vendor's events will not reach the recorder.
# `not installed here` is a vendor this machine does not have, not a broken hook.
hook_problems() { # hook_problems <doctor output>
    printf '%s\n' "$1" | awk '
        /^hooks / && !/ runs / && !/ not installed here/ { print; next }
        /^events / && /MISSING/ { print; next }
        /^ +(NOT TRUSTED|MODIFIED)/ { print }'
}

verdict_hooks() {
    if [ -n "$doctor_failed" ]; then
        line UNCHECKED hooks "$doctor_failed"
        return
    fi
    vh_bad=$(hook_problems "$doctor_out")
    vh_runs=$(printf '%s\n' "$doctor_out" | grep -c '^hooks .* runs ' || true)
    if [ -n "$vh_bad" ]; then
        line FAIL hooks "$(printf '%s\n' "$vh_bad" | sed -n '1p' | sed -E 's/  +/ /g' | cut -c1-170)"
        printf '%s\n' "$vh_bad" | sed -n '2,$p' | sed -E 's/  +/ /g; s/^ */  /' | cut -c1-170
    elif [ "$vh_runs" -eq 0 ]; then
        line UNCHECKED hooks "doctor names no hook that runs here — nothing is registered to record"
    else
        line PASS hooks "$vh_runs hook line(s) runs; what that proves: the command starts and answers, or for cursor and opencode that it is registered. harness-activity is the proof that vendors fire them"
    fi
}

# Ask the recorder something it must answer and will not record. A line that is
# not a request comes back as `failed` from the loop that serves hooks: no record,
# no drop line, no daemon.log line. Connecting proves only that the socket exists;
# a daemon wedged in its accept loop still lets you connect. A busy daemon gets
# three tries before it counts as not serving.
probe_recorder() { # exits 0 answered, 1 connected and no answer, 2 nothing there
    probe_try=0
    while [ "$probe_try" -lt 3 ]; do
        probe_rc=0
        perl -MIO::Socket::UNIX -MIO::Select -e '
            $SIG{PIPE} = "IGNORE";
            my ($path, $limit) = @ARGV;
            my $s = IO::Socket::UNIX->new(Type => SOCK_STREAM(), Peer => $path) or exit 2;
            $s->autoflush(1);
            print $s "{}\n";
            IO::Select->new($s)->can_read($limit) or exit 1;
            sysread($s, my $reply, 4096) or exit 1;
            exit($reply =~ /"status"/ ? 0 : 1);' "$root/conbus.sock" "${CONBUS_CANARY_PROBE_S:-5}" || probe_rc=$?
        [ "$probe_rc" -eq 1 ] || return "$probe_rc"
        probe_try=$((probe_try + 1))
    done
    return 1
}

verdict_recorder() {
    vr_faults=""
    vr_rc=0
    probe_recorder || vr_rc=$?
    case $vr_rc in
        0) ;;
        2) vr_faults="$vr_faults; nothing answers at $root/conbus.sock" ;;
        *) vr_faults="$vr_faults; something holds $root/conbus.sock and did not answer in three tries of ${CONBUS_CANARY_PROBE_S:-5}s" ;;
    esac
    # An idle daemon drains the spool every two seconds, so an old entry is a
    # record that has not reached the log.
    vr_stale=0
    if [ -d "$root/spool" ] && [ ! -r "$root/spool" ]; then
        vr_faults="$vr_faults; $root/spool cannot be read"
    elif [ -d "$root/spool" ]; then
        vr_stale=$(find "$root/spool" -maxdepth 1 -name '*.json' -mmin +10 | wc -l | tr -d ' ')
    fi
    if [ "$vr_stale" -gt 0 ]; then
        vr_faults="$vr_faults; $vr_stale spooled record(s) older than 10 minutes have not reached the log"
    fi
    if [ -n "$vr_faults" ]; then
        line FAIL recorder-serving "${vr_faults#; }"
    else
        line PASS recorder-serving "the daemon answered a request on its socket, and nothing is stuck in the spool"
    fi
}

# Every boot since the swap should be one the script knows about: the swap's own,
# or a planned restart's. Paired by time, so a planned restart that never served
# cannot cancel out a crash, and a log trimmed down to one line cannot read as
# zero restarts.
verdict_restarts() {
    if [ ! -r "$daemon_log" ]; then
        line UNCHECKED restarts "no readable $daemon_log"
        return
    fi
    vs_lines=$(awk -v s="$since" '$1 ~ /^[0-9]/ && $1 > s && /conbus daemon: .*: socket .* log / { print $1 }' "$daemon_log")
    if [ -z "$vs_lines" ]; then
        # The swap itself boots once: a log that names no start since
        # the swap was rotated or truncated underneath it — counting
        # unplanned restarts against a window the log cannot see is
        # checking nothing.
        line UNCHECKED restarts \
            "$daemon_log names no start since $since — the log does not reach back to the swap"
        return
    fi
    vs_starts=""
    for vs_ts in $vs_lines; do
        vs_starts="$vs_starts $(ts_epoch "$vs_ts")"
    done
    vs_planned=""
    for vs_row in $(grep -o '"ts":"[^"]*","serving_ms":-\{0,1\}[0-9]*' "$state_file" 2>/dev/null || true); do
        vs_ts=${vs_row#*'"ts":"'}
        vs_ts=${vs_ts%%'"'*}
        vs_planned="$vs_planned $(ts_epoch "$vs_ts"):${vs_row##*:}:$vs_ts"
    done
    # `start` and `restart` each allow the serving limit for install --apply and
    # again for the boot; a start line later than that is its own boot.
    vs_rows=$(awk -v starts="$vs_starts" -v planned="$vs_planned" -v grace=$((serving_limit * 2 + 30)) \
        -v swap="$(ts_epoch "${recorded_swap:-$since}")" -v exact="$([ "$since" = "$recorded_swap" ] && echo 1)" '
        function take(from,    i) {
            for (i = 1; i <= n; i++)
                if (!(i in used) && st[i] >= from && st[i] <= from + grace) { used[i] = 1; return 1 }
            return 0
        }
        BEGIN {
            n = split(starts, st, " ")
            m = split(planned, pl, " ")
            if (exact == 1) {
                if (!take(swap)) printf "no_swap_boot 1\n"
            } else used[1] = 1
            for (j = 1; j <= m; j++) {
                split(pl[j], part, ":")
                # The stamp is the 3rd field onward: colons in the time split too.
                when = pl[j]; sub(/^[^:]*:[^:]*:/, "", when)
                got = take(part[1])
                if (part[2] < 0) printf "never_served %s\n", when
                else if (!got) printf "no_line %s\n", when
            }
            for (i = 1; i <= n; i++) if (!(i in used)) unplanned++
            printf "seen %d\nplanned %d\nunplanned %d\n", n, m, unplanned
        }')
    vs_seen=$(row_value "$vs_rows" seen)
    vs_count=$(row_value "$vs_rows" planned)
    vs_unplanned=$(row_value "$vs_rows" unplanned)
    vs_never=$(row_value "$vs_rows" never_served | tr '\n' ' ')
    vs_noline=$(row_value "$vs_rows" no_line | tr '\n' ' ')
    vs_newest=$(printf '%s\n' "$vs_lines" | sed -n '$p')
    vs_stamped=$(sed -n 's/.*"started":"\([^"]*\)".*/\1/p' "$stamp" 2>/dev/null || true)

    vs_faults="" vs_open=""
    if [ "$vs_unplanned" -gt 0 ]; then
        vs_faults="$vs_faults; $vs_unplanned unplanned daemon start(s) since $since ($vs_seen seen: 1 swap + $vs_count planned)"
    fi
    if [ -n "$vs_never" ]; then
        vs_faults="$vs_faults; a planned restart never served: $vs_never"
    fi
    if [ -n "$(row_value "$vs_rows" no_swap_boot)" ]; then
        vs_open="$vs_open; the first start line since the swap is at ${vs_lines%%[[:space:]]*}, too late to be the swap's own boot — the log does not reach back to the swap"
    fi
    if [ -n "$vs_noline" ]; then
        vs_open="$vs_open; no start line for the planned restart at: ${vs_noline}— the log was trimmed"
    fi
    if [ -n "$vs_stamped" ] && [ "$vs_stamped" \> "$vs_newest" ]; then
        vs_open="$vs_open; the running daemon's stamp says it started at $vs_stamped, after the newest start line ($vs_newest) — the log was trimmed, or that boot never finished"
    fi
    if [ -n "$vs_faults" ]; then
        line FAIL restarts "${vs_faults#; }${vs_open}"
    elif [ -n "$vs_open" ]; then
        line UNCHECKED restarts "${vs_open#; }"
    else
        line PASS restarts \
            "0 unplanned daemon start(s) since $since ($vs_seen seen: 1 swap + $vs_count planned)"
    fi
}

# Unreadable lines across the window, not the last boot. Each boot reports the whole
# log, so the worst one counts, with doctor's read for what landed after it. A boot
# that could not read the log prints a zero, so that case needs its own look.
verdict_unreadable() {
    vu_rows=""
    if [ -r "$daemon_log" ]; then
        vu_rows=$(awk -v s="$since" '
            $1 ~ /^[0-9]/ && $1 > s {
                if (/starting with no bindings/) failed++
                if (match($0, /\([0-9]+ record\(s\), [0-9]+ unreadable\)/)) {
                    split(substr($0, RSTART + 1, RLENGTH - 2), word, " ")
                    boots++
                    if (word[3] + 0 > worst + 0) worst = word[3]
                    if (boots > 1 && word[1] + 0 < previous + 0) fell = previous " to " word[1]
                    previous = word[1]
                }
            }
            END { printf "boots %d\nworst %d\nfailed %d\nfell %s\n", boots, worst, failed, fell }' "$daemon_log")
    fi
    vu_boots=$(row_value "$vu_rows" boots)
    vu_worst=$(row_value "$vu_rows" worst)
    vu_failed=$(row_value "$vu_rows" failed)
    vu_fell=$(row_value "$vu_rows" fell)
    vu_now=$(printf '%s\n' "$doctor_out" | sed -n 's/^log.* \([0-9][0-9]*\) unreadable line(s)/\1/p' | sed -n '$p')

    if [ "${vu_failed:-0}" -gt 0 ]; then
        line FAIL unreadable "$vu_failed boot(s) since $since could not read the log and started with no bindings"
    elif [ -n "$vu_fell" ]; then
        line FAIL unreadable "the log's record count fell across a restart, from $vu_fell"
    elif [ "${vu_worst:-0}" -gt 0 ] || [ "${vu_now:-0}" -gt 0 ]; then
        line FAIL unreadable "unreadable line(s): worst boot since the swap ${vu_worst:-unknown}, doctor now ${vu_now:-unknown}"
    elif [ -z "$vu_now" ]; then
        line UNCHECKED unreadable "doctor named no count${doctor_failed:+ ($doctor_failed)}; ${vu_boots:-0} boot(s) since the swap reported 0, and they read the log earlier"
    else
        line PASS unreadable "0 unreadable line(s) across ${vu_boots:-0} boot(s) since the swap and in doctor's read now"
    fi
}

# Space, and anything else the log refused. A full disk can take the record, the
# spool entry and the drop line together, and daemon.log is the one trace left when
# that write lands. A build that holds refused records (#155) says so at each end
# instead, and its drop line covers an outage that ended; one that has not ended is
# read here. Doctor's 10% warning stays a note: this Mac lives under it.
verdict_disk() {
    vd_rows=""
    if [ -r "$daemon_log" ]; then
        vd_rows=$(awk -v s="$since" '
            $1 ~ /^[0-9]/ && $1 > s && /record\(s\) refused by the log/ {
                lines++
                if (first == "") { first = $0; sub(/^.*refused by the log: */, "", first) }
                for (i = 1; i <= NF; i++) if ($i == "of") { total += $(i - 1); break }
            }
            $1 ~ /^[0-9]/ && $1 > s && /the log refused a write / {
                refusing = $0
                sub(/^.*the log refused a write /, "", refusing)
            }
            $1 ~ /^[0-9]/ && $1 > s && /the log takes writes again/ { refusing = "" }
            END { printf "lines %d\ntotal %d\nfirst %s\nrefusing %s\n", lines, total, first, refusing }' "$daemon_log")
    fi
    vd_lines=$(row_value "$vd_rows" lines)
    vd_total=$(row_value "$vd_rows" total)
    vd_first=$(row_value "$vd_rows" first | cut -c1-120)
    vd_refusing=$(row_value "$vd_rows" refusing | cut -c1-120)
    vd_line=$(printf '%s\n' "$doctor_out" | sed -n 's/^disk  *//p' | sed -n '1p')
    vd_write=$(printf '%s\n' "$doctor_out" | sed -n 's/^writable  *//p' | sed -n '1p')
    case $vd_write in
        NOT\ APPENDABLE*) vd_write_bad=$vd_write ;;
        *) vd_write_bad="" ;;
    esac
    if [ "${vd_lines:-0}" -gt 0 ]; then
        line FAIL disk "$vd_total record(s) refused by the log since $since, $vd_lines time(s) in daemon.log: $vd_first"
    elif [ -n "$vd_refusing" ]; then
        line FAIL disk "the log refused a write and daemon.log does not say it stopped: $vd_refusing"
    elif [ -n "$vd_write_bad" ]; then
        line FAIL disk "$vd_write_bad"
    elif [ ! -r "$daemon_log" ] || [ -z "$vd_line" ]; then
        line UNCHECKED disk "needs daemon.log and doctor's disk line${doctor_failed:+ ($doctor_failed)}"
    else
        line PASS disk "no record refused by the log since $since, as far as a write to daemon.log could say; now: $vd_line"
    fi
}

# UTC ISO-8601 to epoch milliseconds, the fraction kept. Empty on failure.
ts_ms() {
    tm_epoch=$(ts_epoch "$1")
    is_uint "$tm_epoch" || return 0
    tm_frac=${1#*.}
    tm_frac=${tm_frac%Z}
    case $tm_frac in
        [0-9][0-9][0-9]) ;;
        *) tm_frac=000 ;;
    esac
    echo $((tm_epoch * 1000 + 10#$tm_frac))
}

is_idle() { # is_idle <harness or day>
    case ",$idle," in
        *,"$1",*) return 0 ;;
        *) return 1 ;;
    esac
}

# YYYY-MM-DD -> the next YYYY-MM-DD, on macOS or GNU date.
next_day() {
    if [ "$(uname -s)" = Darwin ]; then
        date -j -v+1d -f "%Y-%m-%d" "$1" +%Y-%m-%d 2>/dev/null
    else
        date -d "$1 +1 day" +%Y-%m-%d 2>/dev/null
    fi
}

# ISO-8601 UTC -> epoch, on macOS or GNU date. Empty on failure. The input
# is always a UTC stamp, so it is parsed as UTC everywhere — an unadorned
# `date -j -f` reads it in local time and shifts every window by the
# zone offset.
ts_epoch() {
    iso=${1%%.*}
    if [ "$(uname -s)" = Darwin ]; then
        TZ=UTC date -j -f "%Y-%m-%dT%H:%M:%S" "$iso" +%s 2>/dev/null || true
    else
        date -u -d "$iso" +%s 2>/dev/null || true
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

    doctor_rc=0
    doctor_out=$(run_doctor) || doctor_rc=$?
    if [ "$doctor_rc" -eq 142 ]; then
        check 1 doctor "no answer inside ${doctor_limit_s}s — a vendor probe is wedged"
        check 2 socket "unknown — doctor did not answer"
        check 2 disk "unknown — doctor did not answer"
        check 2 daemon-build "unknown — doctor did not answer"
    else
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
            *WARNING*)
                if [ "${CONBUS_CANARY_DISK_WARN_OK:-}" = 1 ]; then
                    check 2 disk "$disk_line (accepted by CONBUS_CANARY_DISK_WARN_OK)"
                else
                    check 1 disk "$disk_line"
                fi
                ;;
            *) check 0 disk "${disk_line:-no disk line}" ;;
        esac
        if grep -q 'DIFFERS' <<<"$doctor_out"; then
            check 1 daemon-build "$(grep 'DIFFERS' <<<"$doctor_out" | sed -n '1p')"
        else
            check 0 daemon-build "no DIFFERS"
        fi
    fi

    # S1–S5 evidence, kept outside the repo per the plan. With no
    # --evidence the default ./evidence is only warned about — the
    # operator's evidence may live somewhere else.
    evidence_dir=${evidence:-./evidence}
    if [ -d "$evidence_dir" ]; then
        for i in 1 2 3 4 5; do
            found=$(cd "$evidence_dir" && { ls "s$i"* "S$i"* 2>/dev/null || true; } | sed -n '1p')
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

    # A drop log at the cap on a build that does not rotate has been blind
    # since it filled — swapping over it would carry the blind spot into
    # the new window's verdict. Move it aside first.
    if drops_blind "${from:-$bin}"; then
        check 1 drops-cap \
            "$drops_log is at the 4 MiB cap and blind — rotate it aside before a swap"
    else
        check 0 drops-cap "drops.jsonl below the cap"
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
        if [ "$(uname -s)" = Darwin ] && { is_passwd_home || is_real_env; }; then
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
                # All of it first: `grep -q` leaves at the match, and the
                # SIGPIPE codesign takes for that fails the pipe under pipefail.
                signed=$(codesign -dv --arch "$arch" "$from" 2>&1 || true)
                case $signed in
                    *runtime*) ;;
                    *) runtime_fail="$runtime_fail $arch" ;;
                esac
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
from=""
evidence=""
since=""
idle=""
while [ $# -gt 0 ]; do
    case $1 in
        --live) refuse "--live is gone — a rehearsal is CONBUS_CANARY_REHEARSAL=1 under a throwaway HOME" ;;
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
        --idle)
            [ $# -ge 2 ] || refuse "--idle wants a harness or a day"
            idle="$idle,$2"
            shift
            ;;
        *) refuse "unknown argument: $1" ;;
    esac
    shift
done

is_uint "$doctor_limit_s" && [ "$doctor_limit_s" -ge 1 ] ||
    refuse "CONBUS_CANARY_DOCTOR_LIMIT='$doctor_limit_s' is not a positive integer"

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
            doctor_rc=0
            doctor_out=$(run_doctor 2>/dev/null) || doctor_rc=$?
            if [ "$doctor_rc" -eq 142 ]; then
                say "socket: doctor did not answer inside ${doctor_limit_s}s"
            else
                socket_line=$(echo "$doctor_out" | grep '^socket' || true)
                say "socket: ${socket_line:-doctor gave no socket line}"
            fi
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
        # an unplanned one is still counted.
        #
        # Exits 0 on a completed pass, 1 on a fail, 2 on arguments it refuses,
        # 3 while the 7-day window is still open, 4 when the window is over and
        # an item other than operator-feel could not be judged. Every read that
        # can fail is taken with its status in hand, because `x=$(a | b)` under
        # `set -e` ends the script on the spot and prints no summary at all.
        # Bytes, not characters, for everything this reads: on macOS sed, cut and
        # tr exit non-zero on one invalid byte under a UTF-8 locale, and `set -e`
        # would end the verdict on the line a bad byte sits on.
        export LC_ALL=C
        for answer in $(printf '%s' "$idle" | tr ',' ' '); do
            case $answer in
                [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) ;;
                *[!a-z_]*) refuse "--idle '$answer' is not a harness or a YYYY-MM-DD day" ;;
            esac
        done
        fails=0
        unchecked=""
        line() { # line <PASS|FAIL|UNCHECKED> <name> <detail>
            printf '%s %s — %s\n' "$1" "$2" "$3"
            case $1 in
                FAIL) fails=$((fails + 1)) ;;
                UNCHECKED)
                    # The one question no tool can answer may stay open for a
                    # pass. Anything else left open means nothing was proved.
                    if [ "$2" != operator-feel ]; then
                        unchecked="$unchecked$2 — $3"$'\n'
                    fi
                    ;;
            esac
        }

        # Every comparison below is on this text, so anything else is refused
        # rather than compared: `yesterday` sorts after every timestamp.
        case $since in
            '') ;;
            [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]) since="$since"T00:00:00.000Z ;;
            [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9]Z)
                since=${since%Z}.000Z
                ;;
            [0-9][0-9][0-9][0-9]-[0-9][0-9]-[0-9][0-9]T[0-9][0-9]:[0-9][0-9]:[0-9][0-9].[0-9][0-9][0-9]Z) ;;
            *) refuse "--since '$since' is not YYYY-MM-DD or YYYY-MM-DDTHH:MM:SS[.mmm]Z" ;;
        esac
        if [ -n "$since" ] && ! is_uint "$(ts_epoch "$since")"; then
            refuse "--since '$since' is not a date"
        fi
        if [ -f "$state_file" ] && [ ! -r "$state_file" ]; then
            line UNCHECKED window "$state_file cannot be read — nothing says when the swap was"
            say "verdict: UNCHECKED — no swap time to judge a window from; not a pass"
            exit 4
        fi
        recorded_swap=$(state_get swapped || true)
        if [ -n "$recorded_swap" ] && ! is_uint "$(ts_epoch "$recorded_swap")"; then
            line UNCHECKED window "$state_file says the swap was '$recorded_swap', which is not a time"
            say "verdict: UNCHECKED — no swap time to judge a window from; not a pass"
            exit 4
        fi
        if [ -z "$since" ]; then
            since=$recorded_swap
        fi
        if [ -z "$since" ] && [ -f "$stamp" ]; then
            since=$(sed -n 's/.*"started":"\([^"]*\)".*/\1/p' "$stamp" 2>/dev/null || true)
            say "note: no canary.json — bounding by the daemon's own start $since"
        fi
        [ -n "$since" ] ||
            die "no --since, no canary.json, no daemon.build stamp — nothing to bound the window with"
        since_day=${since:0:10}
        today_utc=$(date -u +%Y-%m-%d)
        operator_answers=""
        say "window: since $since"

        [ -x "$bin" ] || die "no installed conbus at $bin — verdict needs its reads"
        noise_limit=${CONBUS_CANARY_NOISE_MAX:-2000}
        is_uint "$noise_limit" || refuse "CONBUS_CANARY_NOISE_MAX='$noise_limit' is not a number"

        # One doctor for every item that reads it. Each doctor starts Codex's
        # app-server two or three times, so items do not get their own.
        doctor_t0=$(perl -MTime::HiRes=time -e 'printf "%d", time()*1000')
        doctor_rc=0
        doctor_out=$(run_doctor 2>/dev/null) || doctor_rc=$?
        doctor_first_ms=$(($(perl -MTime::HiRes=time -e 'printf "%d", time()*1000') - doctor_t0))
        doctor_failed=""
        if [ "$doctor_rc" -eq 142 ]; then
            doctor_failed="doctor did not answer inside ${doctor_limit_s}s"
        elif [ "$doctor_rc" -ne 0 ]; then
            doctor_failed="doctor exited $doctor_rc"
        elif ! grep -q '^socket' <<<"$doctor_out"; then
            doctor_failed="doctor printed no socket line"
        fi

        verdict_build
        verdict_hooks

        verdict_recorder
        verdict_restarts
        verdict_unreadable
        verdict_disk

        # A read that failed and a log with no rows are different facts, and
        # neither is evidence of activity.
        usage_err=$(mktemp "${TMPDIR:-/tmp}/canary-usage.XXXXXX")
        usage_rc=0
        usage_json=$("$bin" usage --json --since 1970-01-01 2>"$usage_err") || usage_rc=$?
        usage_problem=""
        if [ "$usage_rc" -ne 0 ]; then
            usage_problem="usage --json failed (exit $usage_rc): $(sed -n '1p' "$usage_err" | cut -c1-160)"
        elif [ -z "$usage_json" ]; then
            usage_problem="usage --json gave no rows"
        elif [ -s "$usage_err" ]; then
            say "note: usage --json reported: $(sed -n '1p' "$usage_err" | cut -c1-160)"
        fi
        rm -f "$usage_err"
        # The same read cut at the swap itself, because the swap day's rows hold
        # whatever ran before it. A row without tool_calls is a count not read.
        window_problem=$usage_problem
        window_tool_calls=0
        if [ -z "$window_problem" ]; then
            window_rc=0
            window_json=$("$bin" usage --json --since "$since" 2>/dev/null) || window_rc=$?
            if [ "$window_rc" -ne 0 ]; then
                window_problem="usage --json --since $since failed (exit $window_rc)"
            else
                window_tool_calls=$(echo "$window_json" | awk '
                    NF == 0 { next }
                    match($0, /"tool_calls":[0-9]+/) { total += substr($0, RSTART + 13, RLENGTH - 13); next }
                    { missing++ }
                    END { print (missing ? "missing" : total + 0) }')
                if [ "$window_tool_calls" = missing ]; then
                    window_tool_calls=0
                    window_problem="usage rows name no tool_calls"
                fi
            fi
        fi

        # 3. Drops, then what the same lines say about tool bodies. Both read the
        #    usage rows taken above.
        verdict_drops

        if [ -n "$window_problem" ]; then
            line UNCHECKED tool-bodies \
                "${elided_total:-0} tool event(s) elided, not sealed; no tool-call count to weigh them against: $window_problem"
        elif [ "${elided_bad:-0}" -gt 0 ]; then
            line UNCHECKED tool-bodies \
                "$elided_bad elided line(s) do not read '<N> tool event(s)' — the count is not known"
        elif [ $((${elided_total:-0} * 100)) -gt "$window_tool_calls" ]; then
            line FAIL tool-bodies \
                "${elided_total:-0} tool event(s) elided, not sealed — more than 1% of $window_tool_calls tool call(s); a spool elides tens per restart, a Keychain that will not answer elides them all"
        else
            line PASS tool-bodies \
                "${elided_total:-0} tool event(s) elided, not sealed — within 1% of $window_tool_calls tool call(s)"
        fi

        verdict_activity

        # 5. twins and `repeated` growth vs the pre-swap baseline, same rows.
        if [ -z "$usage_problem" ]; then
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
                    printf "%s %.4f %.4f %d %d %d", (wdays ? (wrate <= brate ? "PASS" : "FAIL") : "UNCHECKED"), wrate, brate, wr, br, wdays
                }')
            set -- $rep_result
            if [ "$1" = UNCHECKED ]; then
                line UNCHECKED twins-repeated \
                    "usage --json has no rows since $since_day — nothing to compare"
            else
                line "$1" twins-repeated \
                    "repeated/day $2 since vs $3 before ($4 vs $5 total)"
            fi
        else
            line UNCHECKED twins-repeated "$usage_problem"
        fi

        # 6. usage-parity: every closed UTC day in the window, the rollback
        #    binary's usage --json --day D against the candidate's, with
        #    the documented tokens_* keys stripped.
        if [ ! -x "$rollback_bin" ]; then
            line FAIL usage-parity "no $rollback_bin — the 0.2.8 side of the parity is missing"
        else
            strip='s/"tokens_[a-z_]*":[0-9]*,?//g; s/,\}/}/g; s/,]/]/g'
            parity_bad=""
            parity_unread=""
            parity_empty=""
            parity_seen=0
            d=$since_day
            guard=0
            while [ "$d" \< "$today_utc" ] && [ "$guard" -lt 370 ]; do
                old_rc=0 new_rc=0
                old=$("$rollback_bin" usage --json --day "$d" 2>/dev/null) || old_rc=$?
                new=$("$bin" usage --json --day "$d" 2>/dev/null) || new_rc=$?
                if [ "$old_rc" -ne 0 ] || [ "$new_rc" -ne 0 ]; then
                    # A side that failed compared nothing, whichever way the
                    # text happens to come out.
                    parity_unread="$parity_unread $d"
                else
                    old=$(echo "$old" | sed -E "$strip")
                    new=$(echo "$new" | sed -E "$strip")
                    if [ "$old" != "$new" ]; then
                        parity_bad="$parity_bad $d"
                    elif [ -z "$old" ]; then
                        parity_empty="$parity_empty $d"
                    else
                        parity_seen=$((parity_seen + 1))
                    fi
                fi
                d=$(next_day "$d") || break
                guard=$((guard + 1))
            done
            if [ -n "$parity_bad" ]; then
                line FAIL usage-parity "usage differs on:$parity_bad"
            elif [ -n "$parity_unread" ]; then
                line UNCHECKED usage-parity "a side could not read:$parity_unread"
            elif [ "$parity_seen" -eq 0 ]; then
                # Two binaries that both answer nothing "agree" on every
                # day: a parity of emptiness proves nothing.
                line UNCHECKED usage-parity \
                    "no closed day produced rows on either side — nothing was compared"
            else
                line PASS usage-parity \
                    "$parity_seen closed day(s) with rows agree (tokens_* stripped)${parity_empty:+; empty on both sides, so not compared:$parity_empty}"
            fi
        fi

        # 7. rss-budget + M3 time budgets: the running recorder's RSS and
        #    three re-measured reads against the plan's targets.
        rss_kb=""
        rss_who=""
        if [ -f "$stamp" ]; then
            pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp" 2>/dev/null || true)
            # `ps` exits 1 for a dead pid, which is the case this item is for.
            if [ -n "$pid" ]; then
                rss_kb=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ' || true)
                rss_who=$(ps -o command= -p "$pid" 2>/dev/null || true)
            fi
        fi
        # A reused pid has a size too. Only a conbus daemon's counts.
        if [ -n "$rss_who" ] && case $rss_who in *conbus*daemon*) false ;; *) true ;; esac; then
            line FAIL rss-budget "pid $pid in the stamp is not a conbus daemon: $(echo "$rss_who" | cut -c1-60)"
        elif is_uint "${rss_kb:-}"; then
            rss_mb=$((rss_kb / 1024))
            if [ "$rss_mb" -le "$rss_budget_mb" ]; then
                line PASS rss-budget "recorder RSS ${rss_mb} MB ≤ ${rss_budget_mb} MB"
            else
                line FAIL rss-budget "recorder RSS ${rss_mb} MB > ${rss_budget_mb} MB"
            fi
        else
            line FAIL rss-budget "no running recorder pid — the daemon itself is not up"
        fi

        measure() { # measure <cmd...> — echoes "<wall-clock ms> <exit status>"
            start_ms=$(perl -MTime::HiRes=time -e 'printf "%d", time()*1000')
            measured_rc=0
            "$@" >/dev/null 2>&1 || measured_rc=$?
            end_ms=$(perl -MTime::HiRes=time -e 'printf "%d", time()*1000')
            echo "$((end_ms - start_ms)) $measured_rc"
        }
        # One slow read under load is the machine, not the build, and a doctor
        # re-read starts its vendor probes again. So a read that makes its budget
        # is read once; one that does not gets two more, and the median judges.
        # A wedge still trips it, a spike does not.
        judge_read() { # judge_read <budget ms> <first ms> <first status> <cmd...> — echoes "<ms> <exit status>"
            judge_budget=$1 judge_ms=$2 judge_rc=$3
            shift 3
            if [ "$judge_ms" = - ]; then
                read -r judge_ms judge_rc <<<"$(measure "$@")"
            fi
            if [ "$judge_rc" -eq 0 ] && [ "$judge_ms" -gt "$judge_budget" ]; then
                judge_again=$(for _ in 1 2; do measure "$@"; done)
                judge_ms=$(printf '%s\n%s\n' "$judge_ms" "$judge_again" | awk '{ print $1 }' | sort -n | sed -n '2p')
                judge_rc=$(printf '%s\n' "$judge_again" | awk '$2 != 0 { rc = $2 } END { print rc + 0 }')
            fi
            echo "$judge_ms $judge_rc"
        }
        read -r t_tail rc_tail <<<"$(judge_read "$tail_ms_budget" - - "$bin" log --tail 1)"
        read -r t_usage rc_usage <<<"$(judge_read "$usage_ms_budget" - - "$bin" usage --days 7)"
        read -r t_doctor rc_doctor <<<"$(judge_read "$doctor_ms_budget" "$doctor_first_ms" "$doctor_rc" run_doctor)"
        tb_failed=""
        [ "$rc_tail" -eq 0 ] || tb_failed="$tb_failed log --tail(exit $rc_tail)"
        [ "$rc_usage" -eq 0 ] || tb_failed="$tb_failed usage(exit $rc_usage)"
        # 142 is run_doctor's cap: a doctor that never answered is over budget,
        # which the times below already say. Any other status failed fast.
        case $rc_doctor in
            0 | 142) ;;
            *) tb_failed="$tb_failed doctor(exit $rc_doctor)" ;;
        esac
        tb_bad=""
        [ "$t_tail" -gt "$tail_ms_budget" ] && tb_bad="$tb_bad tail(${t_tail}>${tail_ms_budget})"
        [ "$t_usage" -gt "$usage_ms_budget" ] && tb_bad="$tb_bad usage(${t_usage}>${usage_ms_budget})"
        [ "$t_doctor" -gt "$doctor_ms_budget" ] && tb_bad="$tb_bad doctor(${t_doctor}>${doctor_ms_budget})"
        # A cap lower than the budget must not turn a doctor that never answered
        # into one that answered in time.
        [ "$rc_doctor" -eq 142 ] && tb_bad="$tb_bad doctor(no answer inside ${doctor_limit_s}s)"
        if [ -n "$tb_failed" ]; then
            # A read that fails fast beats any budget and proves none of them.
            line UNCHECKED time-budget "a failing read cannot be timed:$tb_failed"
        elif [ -z "$tb_bad" ]; then
            line PASS time-budget \
                "log --tail 1 ${t_tail}ms≤${tail_ms_budget} usage --days 7 ${t_usage}ms≤${usage_ms_budget} doctor ${t_doctor}ms≤${doctor_ms_budget}"
        else
            load_now=$(LC_ALL=C uptime | sed -E 's/.*load averages?: *//' || true)
            line FAIL time-budget \
                "over budget on:$tb_bad (median of 3; load $load_now — re-run when the machine is quiet to tell the build from the load)"
        fi

        # 8. Serving ≤ 5 s on the newest recorded boot — the goal §7 moved
        #    here from the rollback checks.
        if [ -f "$stamp" ]; then
            started=$(sed -n 's/.*"started":"\([^"]*\)".*/\1/p' "$stamp" 2>/dev/null || true)
            serving=$(sed -n 's/.*"serving":"\([^"]*\)".*/\1/p' "$stamp" 2>/dev/null || true)
            e0=$(ts_ms "$started") e1=$(ts_ms "$serving")
            if is_uint "${e0:-}" && is_uint "${e1:-}"; then
                serving_ms=$((e1 - e0))
                # Shown, not judged: one reading under load cannot be taken again.
                recorded_servings=$(grep -o '"serving_ms":-\{0,1\}[0-9]*' "$state_file" 2>/dev/null | cut -d: -f2 | tr '\n' ' ' || true)
                if [ "$serving_ms" -le "$serving_ms_budget" ]; then
                    line PASS serving-budget "start->serving ${serving_ms} ms ≤ ${serving_ms_budget}${recorded_servings:+; planned restarts recorded: ${recorded_servings}ms}"
                else
                    line FAIL serving-budget "start->serving ${serving_ms} ms > ${serving_ms_budget}"
                fi
            else
                line UNCHECKED serving-budget "stamp has no serving pair to measure"
            fi
        else
            line UNCHECKED serving-budget "no daemon.build stamp"
        fi

        # 9. Policy decisions: the file absent means none can be made. A
        #    file that cannot be a policy decides nothing — zero counted
        #    denials would read as a pass over a guard that never ran.
        if [ ! -f "$root/policy.json" ]; then
            line PASS policy "no policy.json — 0 decisions possible"
        elif ! "$bin" policy check >/dev/null 2>&1; then
            line FAIL policy "policy.json cannot be a policy — it decides nothing"
        elif [ -n "$usage_problem" ]; then
            line UNCHECKED policy "policy.json present but $usage_problem"
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

        # The window is open for CONBUS_CANARY_WINDOW_DAYS (default 7), counted
        # from the recorded swap. --since moves what the items read, never the
        # clock: an older date cannot complete a window that is still running,
        # and a look at part of a window is not that window's verdict.
        if [ -n "$recorded_swap" ] && [ "$since" != "$recorded_swap" ]; then
            line UNCHECKED window \
                "--since $since is not the recorded swap $recorded_swap — a look at part of the window, not its verdict"
        elif [ -z "$recorded_swap" ]; then
            line UNCHECKED window \
                "no recorded swap — nothing says $since is where a $window_days-day window began"
        fi
        window_epoch=$(ts_epoch "${recorded_swap:-$since}")
        unchecked_names=$(printf '%s' "$unchecked" | sed 's/ — .*//' | tr '\n' ' ')
        # A fail inside the window still exits 1 — 3 only means "not done yet".
        if [ "$fails" -gt 0 ]; then
            say "verdict: FAIL ($fails failing item(s))${unchecked_names:+; also not judged: $unchecked_names}"
            exit 1
        elif ! is_uint "${window_epoch:-}"; then
            say "verdict: UNCHECKED — '${recorded_swap:-$since}' cannot be read as a time; not a pass"
            exit 4
        elif [ $((($(date +%s) - window_epoch) / 86400)) -lt "$window_days" ]; then
            say "verdict: window incomplete ($(( ($(date +%s) - window_epoch) / 86400 ))/${window_days} days) — not a pass yet${unchecked_names:+; also not judged so far: $unchecked_names}"
            exit 3
        elif [ -n "$unchecked" ]; then
            say "verdict: UNCHECKED — not a pass; these could not be judged:"
            printf '%s' "$unchecked" | sed 's/^/canary:   /'
            exit 4
        fi
        if [ -n "$operator_answers" ]; then
            say "verdict: PASS — on the operator's word that these were idle:$operator_answers; operator-feel is still yours to answer"
        else
            say "verdict: PASS — every item judged; operator-feel is still yours to answer"
        fi
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
        before_ts=$(now_iso_ms)
        ts=$(date -u +%Y-%m-%dT%H:%M:%S.000Z)
        if [ "$mode" = rehearsal ]; then
            say "rehearsal: 'conbus install --apply' skipped (it would hit launchctl)"
            state_add_restart "$ts" -1 0
            say "recorded a planned restart at $ts (rehearsal — no reload)"
            exit 0
        fi
        if ! "$bin" install --apply; then
            die "conbus install --apply failed — restart not recorded; check 'canary.sh status'"
        fi
        serving_ms=-1
        if wait_for_serving "$old_stamp_pid"; then
            started=$(sed -n 's/.*"started":"\([^"]*\)".*/\1/p' "$stamp")
            serving=$(sed -n 's/.*"serving":"\([^"]*\)".*/\1/p' "$stamp")
            e0=$(ts_ms "$started") e1=$(ts_ms "$serving")
            if is_uint "${e0:-}" && is_uint "${e1:-}"; then
                serving_ms=$((e1 - e0))
            fi
        else
            say "restart: no new serving stamp inside ${serving_limit}s — check 'canary.sh status'"
        fi
        # new_drops is the number of drop lines the restart added that lost a
        # record — the plan's "유실 0", not a new-reason count. A sessionless
        # refusal is the vendor's payload, the end of a session the log never
        # had loses nothing, and an elided tool body is a record that landed:
        # excluded, shown aside. A timestamp cut: a rotation can rename the file
        # mid-restart, so line numbers no longer mark "since".
        new_drops=0 sessionless=0 not_lost=0
        while IFS=$'\t' read -r cls _; do
            case $cls in
                '') ;;
                sessionless) sessionless=$((sessionless + 1)) ;;
                ghost | payload | elided | held) not_lost=$((not_lost + 1)) ;;
                *) new_drops=$((new_drops + 1)) ;;
            esac
        done <<<"$(drop_classes_after "$before_ts")"
        state_add_restart "$ts" "$serving_ms" "$new_drops"
        extra=""
        [ "$sessionless" -gt 0 ] && extra=" ($sessionless sessionless refusal(s) not counted)"
        [ "$not_lost" -gt 0 ] && extra="$extra ($not_lost line(s) that lost no record not counted)"
        say "recorded planned restart at $ts: serving ${serving_ms} ms, $new_drops drop(s)$extra"
        ;;

    start)
        # The swap guard decides before any file check: a rehearsal under
        # the real HOME refuses even where no conbus is installed at all —
        # dying on a missing binary would be the wrong verdict for the
        # condition the guard exists to catch.
        mode=$(swap_mode start)
        [ -n "$from" ] || refuse "start wants --from <candidate-binary>"
        [ -x "$from" ] || die "candidate is not an executable: $from"
        [ -x "$bin" ] || die "no installed conbus at $bin — canary only covers an install.sh install"
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
        # A rotation can rename drops.jsonl mid-swap, so "drops since now"
        # is a timestamp cut across both generations, not a line offset.
        before_ts=$(now_iso_ms)

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
            doctor_rc=0
            doctor_out=$(run_doctor) || doctor_rc=$?
            if [ "$doctor_rc" -eq 142 ]; then
                fail_reason="doctor did not answer inside ${doctor_limit_s}s"
            elif grep -q 'DIFFERS' <<<"$doctor_out"; then
                fail_reason="the answering daemon is a different build than $cand_build"
            elif ! grep -q 'listening' <<<"$doctor_out"; then
                fail_reason="the daemon is not answering after the swap"
            else
                bad_hooks=$(awk '/^hooks / && !/ runs /' <<<"$doctor_out" | sed -n '1p')
                if [ -n "$bad_hooks" ]; then
                    fail_reason="a hook is not 'runs': ${bad_hooks#hooks     }"
                fi
            fi
        fi

        swap_window_drops=0
        if [ -z "$fail_reason" ]; then
            while IFS=$'\t' read -r cls r; do
                case $cls in
                    '') ;;
                    swap) swap_window_drops=$((swap_window_drops + 1)) ;;
                    # A spool drained at the first boot writes these; none is a
                    # lost record, and none should undo a healthy swap.
                    ghost | sessionless | payload | elided | held) ;;
                    *)
                        if ! LC_ALL=C grep -qxF "$r" "$before_drops"; then
                            fail_reason="new drop reason(s) since the swap: $r"
                            break
                        fi
                        ;;
                esac
            done <<<"$(drop_classes_after "$before_ts")"
        fi
        if [ "$swap_window_drops" -gt 0 ]; then
            say "start: $swap_window_drops swap-window drop(s) — the old daemon" \
                "refused the new hook mid-restart; expected, counted, not rolled back"
        fi
        rm -f "$before_drops"

        if [ -n "$fail_reason" ]; then
            say "start: $fail_reason — rolling back"
            cand_pid=""
            if [ -f "$stamp" ]; then
                cand_pid=$(sed -n 's/.*"pid":\([0-9]*\).*/\1/p' "$stamp")
            fi
            mv -f "$rollback_bin" "$bin"
            reregister_daemon "$mode" || true
            # The rollback happened whether or not it boots — record it
            # before verification, which exits 3 when it cannot confirm.
            state_write "$swapped_ts" "$prev_build" "$cand_build" "$cand_sha" '[]' \
                "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)"
            say "rolled back $bin to $("$bin" --version 2>&1 || echo unknown)"
            say_spool
            verify_rollback "$prev_build" "$cand_pid" "$mode"
            die "start failed: $fail_reason (rolled back automatically)"
        fi

        if [ "$swap_window_drops" -gt 0 ]; then
            state_write "$swapped_ts" "$prev_build" "$cand_build" "$cand_sha" '[]' \
                "" "$swap_window_drops"
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
        cand_pid=$old_stamp_pid
        mv -f "$rollback_bin" "$bin"
        restored_version=$("$bin" --version 2>&1 || echo unknown)
        say "rolled back $bin to $restored_version ($mode)"
        prev_build=$(echo "$restored_version" | sed -n 's/.*(\([0-9a-f]*\)).*/\1/p')
        reregister_daemon "$mode" || true
        verify_rollback "$prev_build" "$cand_pid" "$mode"
        say_spool
        ;;

    "" | -h | --help | help)
        sed -n '3,10p' "$0"
        ;;
    *)
        refuse "unknown subcommand: $sub (preflight|status|verdict|start|restart|rollback)"
        ;;
esac
