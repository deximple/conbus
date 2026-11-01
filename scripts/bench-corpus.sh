#!/bin/sh
# Measure the M3 table — read latency and start-up cost — on a copy of a real
# data root.
#
#   scripts/bench-corpus.sh OLD NEW ROOT
#
# OLD and NEW are paths to conbus binaries; ROOT is a data root directory
# containing log/ — an APFS clone of a real root, never the live one. Each
# binary gets its own fresh clone (cp -cR), so the two never see each other's
# writes; both clones are deleted at the end.
#
# WARNING: the clones hold real prompts. Every command's stdout and stderr is
# discarded for that reason — only numbers are printed. A daemon start reads
# the real vendor transcripts for gap fill and may start `codex app-server`;
# everything it writes lands in the clone.
#
# The M3 "start -> serving" number is the real-restart row: serve, write a
# few records the way a hook does, TERM, then time the next start. Numbers
# for the plan come from release builds only — a debug binary says nothing
# about what ships.
set -eu

if [ $# -ne 3 ]; then
    echo "usage: $0 OLD NEW ROOT" >&2
    exit 2
fi
OLD=$1
NEW=$2
ROOT=$3

for bin in "$OLD" "$NEW"; do
    [ -x "$bin" ] || {
        echo "$0: not an executable: $bin" >&2
        exit 2
    }
done
[ -d "$ROOT/log" ] || {
    echo "$0: $ROOT has no log/ — not a conbus data root" >&2
    exit 2
}

# The live root is never a corpus. Resolve through symlinks before comparing.
root_real=$(cd "$ROOT" && pwd -P)
state_home=${XDG_STATE_HOME:-"$HOME/.local/state"}
for live in "$HOME/.conbus" "$state_home/conbus"; do
    if [ -d "$live" ] && [ "$(cd "$live" && pwd -P)" = "$root_real" ]; then
        echo "$0: $ROOT is the live data root — refusing" >&2
        exit 2
    fi
done

CLONE1=/tmp/cb-b1
CLONE2=/tmp/cb-b2
rm -rf "$CLONE1" "$CLONE2"
trap 'rm -rf /tmp/cb-b1 /tmp/cb-b2' EXIT

tfile=$(mktemp "${TMPDIR:-/tmp}/cb-bench-time.XXXXXX")
trap 'rm -rf /tmp/cb-b1 /tmp/cb-b2; rm -f "$tfile"' EXIT

now() { perl -MTime::HiRes=time -e 'printf "%.3f\n", time()'; }

# Any reply line counts as serving: the daemon answers junk with a Failed
# response.
serving_probe() {
    perl -MIO::Socket::UNIX -e '
        my $ok = eval {
            local $SIG{ALRM} = sub { die };
            alarm 2;
            my $s = IO::Socket::UNIX->new(Peer => $ARGV[0]) or die;
            print $s "{}\n";
            my $line = <$s>;
            alarm 0;
            defined($line) && length($line) ? 1 : 0;
        };
        exit($ok ? 0 : 1);
    ' "$1" </dev/null >/dev/null 2>&1
}

# A build older than conbus's sign-in check runs `agy -p /usage` from doctor,
# and with no login that opens a browser tab for OAuth.
"${CONBUS_AGY_BIN:-agy}" models >/dev/null 2>&1 </dev/null || export CONBUS_AGY_BIN=/usr/bin/false

# run_timed BIN CLONE args... -> prints "median_seconds max_rss_bytes"
run_timed() {
    bin=$1
    clone=$2
    shift 2
    env CONBUS_HOME="$clone" CONBUS_SKIP_LAUNCHCTL=1 "$bin" "$@" \
        </dev/null >/dev/null 2>&1 || true
    times=""
    max_rss=0
    i=0
    while [ $i -lt 3 ]; do
        i=$((i + 1))
        env CONBUS_HOME="$clone" CONBUS_SKIP_LAUNCHCTL=1 \
            /usr/bin/time -l "$bin" "$@" </dev/null >/dev/null 2>"$tfile" || true
        real=$(awk '/real/{print $1}' "$tfile")
        rss=$(awk '/maximum resident set size/{print $1}' "$tfile")
        times="$times $real"
        [ "${rss:-0}" -gt "$max_rss" ] && max_rss=$rss
    done
    median=$(printf '%s\n' $times | sort -n | sed -n 2p)
    echo "$median $max_rss"
}

# run_daemon BIN CLONE -> prints "serve_seconds|rss_at_serving_kb|rss_5s_kb|note"
run_daemon() {
    bin=$1
    clone=$2
    sock="$clone/conbus.sock"
    rm -f "$sock"
    t0=$(now)
    env CONBUS_HOME="$clone" CONBUS_SKIP_LAUNCHCTL=1 "$bin" daemon \
        </dev/null >/dev/null 2>&1 &
    pid=$!
    note=""
    while :; do
        if serving_probe "$sock"; then
            break
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            note="daemon exited before serving"
            break
        fi
        elapsed=$(perl -e 'printf "%d", time() - $ARGV[0]' "$t0")
        [ "$elapsed" -ge 300 ] && {
            note="timeout"
            break
        }
        sleep 0.05
    done
    if [ -z "$note" ]; then
        t1=$(now)
        serve=$(perl -e 'printf "%.3f", $ARGV[0] - $ARGV[1]' "$t1" "$t0")
        rss1=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')
        sleep 5
        rss2=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')
    else
        serve=$note
        rss1=""
        rss2=""
    fi
    kill -TERM "$pid" 2>/dev/null || true
    n=0
    while kill -0 "$pid" 2>/dev/null && [ $n -lt 200 ]; do
        sleep 0.1
        n=$((n + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
        echo "$0: daemon pid $pid still running after TERM" >&2
    fi
    wait "$pid" 2>/dev/null || true
    echo "$serve|${rss1:-0}|${rss2:-0}|$note"
}

# A UserPromptSubmit payload with no real content: three of these through the
# socket is what separates a warm restart from a real one — the daemon's own
# writes move the log tip past whatever a prepare covered.
HOOK_PAYLOAD='{"session_id":"00000000-0000-4000-8000-0000000000b1","transcript_path":"/tmp/bench-none.jsonl","cwd":"/tmp/bench","prompt_id":"00000000-0000-4000-8000-0000000000b2","permission_mode":"auto","hook_event_name":"UserPromptSubmit","prompt":"bench prompt, not a real one"}'

# run_restart BIN CLONE -> prints "serve_seconds|rss_at_serving_kb|note"
#
# What a user restart actually costs: first serve, write a few records the
# way a hook does, TERM, then time the next start until serving. The first
# start's number is not this one — a daemon that wrote even one line since
# its checkpoint (or sidecar coverage) takes the full prepare path.
run_restart() {
    bin=$1
    clone=$2
    sock="$clone/conbus.sock"
    rm -f "$sock"
    env CONBUS_HOME="$clone" CONBUS_SKIP_LAUNCHCTL=1 "$bin" daemon \
        </dev/null >/dev/null 2>&1 &
    pid=$!
    n=0
    until serving_probe "$sock" || ! kill -0 "$pid" 2>/dev/null || [ $n -ge 6000 ]; do
        sleep 0.05
        n=$((n + 1))
    done
    if ! kill -0 "$pid" 2>/dev/null; then
        echo "daemon exited before serving|0|first start died"
        return
    fi
    i=0
    while [ $i -lt 3 ]; do
        i=$((i + 1))
        printf '%s\n' "$HOOK_PAYLOAD" | env CONBUS_HOME="$clone" \
            CONBUS_SKIP_LAUNCHCTL=1 "$bin" hook --harness claude_code \
            >/dev/null 2>&1 || true
    done
    kill -TERM "$pid" 2>/dev/null || true
    wait "$pid" 2>/dev/null || true

    rm -f "$sock"
    t0=$(now)
    env CONBUS_HOME="$clone" CONBUS_SKIP_LAUNCHCTL=1 "$bin" daemon \
        </dev/null >/dev/null 2>&1 &
    pid=$!
    note=""
    while :; do
        if serving_probe "$sock"; then
            break
        fi
        if ! kill -0 "$pid" 2>/dev/null; then
            note="daemon exited before serving"
            break
        fi
        elapsed=$(perl -e 'printf "%d", time() - $ARGV[0]' "$t0")
        [ "$elapsed" -ge 300 ] && {
            note="timeout"
            break
        }
        sleep 0.05
    done
    if [ -z "$note" ]; then
        t1=$(now)
        serve=$(perl -e 'printf "%.3f", $ARGV[0] - $ARGV[1]' "$t1" "$t0")
        rss1=$(ps -o rss= -p "$pid" 2>/dev/null | tr -d ' ')
    else
        serve=$note
        rss1=""
    fi
    kill -TERM "$pid" 2>/dev/null || true
    n=0
    while kill -0 "$pid" 2>/dev/null && [ $n -lt 200 ]; do
        sleep 0.1
        n=$((n + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
        echo "$0: daemon pid $pid still running after TERM" >&2
    fi
    wait "$pid" 2>/dev/null || true
    echo "$serve|${rss1:-0}|$note"
}

mb() { perl -e 'printf "%.1f", $ARGV[0] / 1048576' "$1"; }

printf '%-30s %12s %12s\n' metric OLD NEW

for side in 1 2; do
    if [ $side -eq 1 ]; then
        bin=$OLD
        clone=$CLONE1
    else
        bin=$NEW
        clone=$CLONE2
    fi
    cp -cR "$ROOT" "$clone"

    set -- $(run_timed "$bin" "$clone" log --tail 1)
    eval "tail_s$side=\$1 tail_rss$side=\$2"
    set -- $(run_timed "$bin" "$clone" usage --days 7)
    eval "usage_s$side=\$1 usage_rss$side=\$2"
    set -- $(run_timed "$bin" "$clone" doctor)
    eval "doctor_s$side=\$1 doctor_rss$side=\$2"

    result=$(run_daemon "$bin" "$clone")
    serve=$(echo "$result" | cut -d'|' -f1)
    rss_s=$(echo "$result" | cut -d'|' -f2)
    rss_5=$(echo "$result" | cut -d'|' -f3)
    eval "serve$side=\$serve dserv$side=\$rss_s d5$side=\$rss_5"

    result=$(run_restart "$bin" "$clone")
    eval "restart$side=$(echo "$result" | cut -d'|' -f1) drss$side=$(echo "$result" | cut -d'|' -f2)"
done

printf '%-30s %12s %12s\n' "log --tail 1 median s" "$tail_s1" "$tail_s2"
printf '%-30s %12s %12s\n' "log --tail 1 max RSS MB" "$(mb "$tail_rss1")" "$(mb "$tail_rss2")"
printf '%-30s %12s %12s\n' "usage --days 7 median s" "$usage_s1" "$usage_s2"
printf '%-30s %12s %12s\n' "usage --days 7 max RSS MB" "$(mb "$usage_rss1")" "$(mb "$usage_rss2")"
printf '%-30s %12s %12s\n' "doctor median s" "$doctor_s1" "$doctor_s2"
printf '%-30s %12s %12s\n' "doctor max RSS MB" "$(mb "$doctor_rss1")" "$(mb "$doctor_rss2")"
printf '%-30s %12s %12s\n' "start -> serving s" "$serve1" "$serve2"
printf '%-30s %12s %12s\n' "daemon RSS at serving MB" "$(mb "$((${dserv1:-0} * 1024))")" "$(mb "$((${dserv2:-0} * 1024))")"
printf '%-30s %12s %12s\n' "daemon RSS +5 s MB" "$(mb "$((${d51:-0} * 1024))")" "$(mb "$((${d52:-0} * 1024))")"
printf '%-30s %12s %12s\n' "real restart -> serving s" "$restart1" "$restart2"
printf '%-30s %12s %12s\n' "restart RSS at serving MB" "$(mb "$((${drss1:-0} * 1024))")" "$(mb "$((${drss2:-0} * 1024))")"
