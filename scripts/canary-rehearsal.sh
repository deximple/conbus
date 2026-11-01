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

# A fake conbus. $2 picks the doctor persona the scenario needs:
#   silent   — socket not answering (the daemon is down or absent)
#   live     — socket listening, matching daemon build, hooks all `runs`
#   differs  — the answering daemon is another build
#   hookfail — a hooks line that is not `runs`
#   dropper  — like live, but drops a new reason into drops.jsonl
write_fake() {
    tag=$1
    persona=${2:-silent}
    path=$3
    case $persona in
        silent) doctor='echo "socket    $HOME/.conbus/conbus.sock — not answering"; echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"; echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"' ;;
        live) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"; echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"; echo "hooks     claude_code  runs — $HOME/.local/bin/conbus hook"; echo "events    claude_code  13/13 registered"' ;;
        differs) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build aaaa pid 9999 since 2026-11-01T10:00:00.000Z — DIFFERS from this binary ('"$tag"'); restart the daemon"' ;;
        hookfail) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     codex        MISSING — /h hooks.json has no conbus entry"' ;;
        dropper) doctor='echo "{\"ts\":\"2030-01-01T00:00:00.000Z\",\"reason\":\"post-swap fault\"}" >> "$HOME/.conbus/drops.jsonl"; echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     claude_code  runs — x"' ;;
    esac
    cat >"$path" <<EOF
#!/bin/sh
case "\$1" in
    --version) echo "conbus 0.0.0-$tag ($tag)" ;;
    doctor) $doctor ;;
    log) echo "cache     0 closed segment(s): 0 built, 0 already valid" ;;
    usage)
        echo '{"day":"2026-10-30","harness":"claude_code","tool_denials":0,"repeated":0,"tokens_input":1}'
        echo '{"day":"2026-10-31","harness":"codex","tool_denials":0,"repeated":0,"tokens_output":2}'
        echo '{"day":"2026-11-01","harness":"codex","tool_denials":0,"repeated":0,"tokens_output":3}'
        ;;
    install) touch "\$HOME/.install-called" ;;
    *) echo "fake-$tag: \$*" ;;
esac
EOF
    chmod 755 "$path"
}

# --- fixtures ---------------------------------------------------------------

write_fake 01d silent "$install_dir/conbus"          # the installed "0.2.8"
write_fake beef live "$tmp/stage/conbus-good"        # a healthy candidate
write_fake beef differs "$tmp/stage/conbus-differs"
write_fake beef hookfail "$tmp/stage/conbus-hookfail"
write_fake beef dropper "$tmp/stage/conbus-dropper"
write_fake beef silent "$tmp/stage/conbus-noserve"   # daemon never serves

cat >"$home/.conbus/daemon.build" <<'EOF'
{"build":"01d","pid":4242,"started":"2026-10-31T10:00:00.000Z","serving":"2026-10-31T10:00:04.000Z"}
EOF
cat >"$home/.conbus/daemon.log" <<'EOF'
2026-10-31T10:00:04.000Z conbus daemon: 01d: socket /x/sock log /x/log (10 record(s), 0 unreadable)
EOF
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
EOF
touch "$home/Library/LaunchAgents/sh.conbus.daemon.plist"
for i in 1 2 3 4 5; do touch "$tmp/evidence/s$i.txt"; done

run() {
    env -u CONBUS_HOME -u XDG_STATE_HOME \
        HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 CONBUS_CANARY_REHEARSAL=1 \
        CONBUS_CANARY_SERVING_LIMIT=${SERVING_LIMIT:-30} \
        bash "$canary" "$@"
}

run_norehearsal() {
    env -u CONBUS_HOME -u XDG_STATE_HOME \
        HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
        CONBUS_CANARY_SERVING_LIMIT=${SERVING_LIMIT:-30} \
        bash "$canary" "$@"
}

expect() { # expect <want> <got> <what>
    if [ "$1" != "$2" ]; then
        echo "rehearsal: FAIL: $3 — want $1, got $2" >&2
        exit 1
    fi
    echo "rehearsal: ok: $3"
}

expect_file() {
    [ -f "$1" ] || { echo "rehearsal: FAIL: $2" >&2; exit 1; }
    echo "rehearsal: ok: $2"
}

expect_absent() {
    [ ! -e "$1" ] || { echo "rehearsal: FAIL: $2" >&2; exit 1; }
    echo "rehearsal: ok: $2"
}

expect_grep() { # expect_grep <pattern> <file> <what>
    if ! grep -q "$1" "$2"; then
        echo "rehearsal: FAIL: $3 — no '$1' in $2:" >&2
        cat "$2" >&2
        exit 1
    fi
    echo "rehearsal: ok: $3"
}

# Simulate the daemon coming up after install --apply: writes a new stamp
# with a new pid and a `serving` field, like the real boot does.
daemon_boot() { # daemon_boot <build> <pid> [delay]
    (sleep "${3:-0.4}"
     [ -d "$home/.conbus" ] || exit 0
     printf '{"build":"%s","pid":%s,"started":"%s","serving":"%s","core_dumps_disabled":true,"memory_locked":true}\n' \
         "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)" "$(date -u +%Y-%m-%dT%H:%M:%S.002Z)" \
         >"$home/.conbus/daemon.build") &
}

say() { printf 'rehearsal: %s\n' "$*"; }

# --- read-only and preflight -------------------------------------------------

run_norehearsal status >/dev/null
expect 0 $? "status is read-only"
run preflight >/dev/null
expect 0 $? "preflight passes on a healthy fake install"
run preflight --evidence "$tmp/evidence" >"$tmp/preflight.out"
expect 0 $? "preflight passes with --evidence"
expect_grep '^PASS evidence-s5' "$tmp/preflight.out" "s1–s5 markers found in --evidence dir"
expect_file "$home/Library/LaunchAgents/sh.conbus.daemon.plist.bak" \
    "preflight backed the plist up beside itself"

# A DIFFERS from the installed doctor fails preflight.
write_fake bad differs "$install_dir/conbus"
code=0
run preflight >/dev/null 2>&1 || code=$?
expect 1 "$code" "preflight fails on DIFFERS"
write_fake 01d silent "$install_dir/conbus"

# Under a rehearsal HOME the signature check warns, does not fail — the fake
# candidate is a shell script and can never carry a signature.
code=0
run preflight --from "$tmp/stage/conbus-good" >"$tmp/preflight-sig.out" 2>&1 || code=$?
expect 0 "$code" "preflight with a fake candidate warns instead of failing"
expect_grep '^WARN signature' "$tmp/preflight-sig.out" "signature check is WARN under rehearsal"
expect_grep '^WARN artifact-hash' "$tmp/preflight-sig.out" "no sidecar warns, does not fail"
expect_grep '^PASS cache' "$tmp/preflight-sig.out" "the candidate prebuilds the cache"
shasum -a 256 "$tmp/stage/conbus-good" | awk '{print $1}' >"$tmp/stage/conbus-good.sha256"
run preflight --from "$tmp/stage/conbus-good" >"$tmp/preflight-hash.out" 2>&1 || true
expect_grep '^PASS artifact-hash' "$tmp/preflight-hash.out" "a matching .sha256 sidecar passes"

# --- the swap guard ----------------------------------------------------------

code=0
run_norehearsal start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start without rehearsal env or --live refuses (exit 2)"
code=0
run_norehearsal rollback >/dev/null 2>&1 || code=$?
expect 2 "$code" "rollback without rehearsal env or --live refuses (exit 2)"
code=0
run_norehearsal restart >/dev/null 2>&1 || code=$?
expect 2 "$code" "restart without rehearsal env or --live refuses (exit 2)"

# Blackouts: an epoch window covering now.
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="1000000000-9999999999" \
    bash "$canary" start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start refuses inside an epoch blackout"

# A daily HH:MM-HH:MM window covering now.
lo=$(date -v-1H +%H:%M 2>/dev/null || date -d '1 hour ago' +%H:%M)
hi=$(date -v+1H +%H:%M 2>/dev/null || date -d '+1 hour' +%H:%M)
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="$lo-$hi" \
    bash "$canary" start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start refuses inside an HH:MM-HH:MM blackout"

# The new `[days] HH:MM-HH:MM [Zone]` form — inside and outside. The
# window is built in the spec's own zone (UTC) so it always covers "now".
today=$(env TZ=UTC date +%u)  # weekday right now in UTC
lo_u=$(env TZ=UTC date -v-1H +%H:%M 2>/dev/null || env TZ=UTC date -d '1 hour ago' +%H:%M)
hi_u=$(env TZ=UTC date -v+1H +%H:%M 2>/dev/null || env TZ=UTC date -d '+1 hour' +%H:%M)
case $today in
    1) dayspec="Mon" ;; 2) dayspec="Tue" ;; 3) dayspec="Wed" ;; 4) dayspec="Thu" ;;
    5) dayspec="Fri" ;; 6) dayspec="Sat" ;; 7) dayspec="Sun" ;;
esac
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="$dayspec $lo_u-$hi_u UTC" \
    bash "$canary" start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 2 "$code" "start refuses inside a 'Day HH:MM-HH:MM Zone' blackout"
# Same window, wrong day — does not block.
case $today in
    1) other="Wed" ;; *) other="Mon" ;;
esac
code=0
daemon_boot beef 5150
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="$other $lo_u-$hi_u UTC" \
    bash "$canary" start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 0 "$code" "a 'Day HH:MM-HH:MM Zone' blackout outside its day does not block"
# An unparseable spec is still a blackout — and says it is unparseable.
code=0
out=$(env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="unparseable" \
    bash "$canary" start --from "$tmp/stage/conbus-good" 2>&1) || code=$?
expect 2 "$code" "start refuses on an unparseable blackout spec"
printf '%s' "$out" >"$tmp/unparseable.out"
expect_grep "unparseable" "$tmp/unparseable.out" "the refusal says the spec is unparseable"
# A Mon-Fri Seoul window — the report's operator command — parses.
case $(env TZ=Asia/Seoul date +%u) in
    1|2|3|4|5) mf_expect=2 ;; *) mf_expect=0 ;;
esac
now_seoul=$(env TZ=Asia/Seoul date +%H:%M)
if [ "$mf_expect" = 2 ]; then
    lo_s=$(env TZ=Asia/Seoul date -v-1H +%H:%M 2>/dev/null || echo "00:00")
    hi_s=$(env TZ=Asia/Seoul date -v+1H +%H:%M 2>/dev/null || echo "23:59")
else
    lo_s="00:00"; hi_s="00:01"
fi
code=0
daemon_boot beef 5151
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_CANARY_BLACKOUT="Mon-Fri $lo_s-$hi_s Asia/Seoul" \
    bash "$canary" start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect "$mf_expect" "$code" "'Mon-Fri HH:MM-HH:MM Asia/Seoul' parses and applies"
[ "$mf_expect" = 0 ] && expect "conbus 0.0.0-beef (beef)" "$("$install_dir/conbus" --version)" "and a weekend swap goes through"

# --- the healthy start -------------------------------------------------------

# A good candidate: serving stamp appears, doctor agrees, hooks run, no
# new drops — the swap stays.
daemon_boot beef 4243
code=0
run start --from "$tmp/stage/conbus-good" >"$tmp/start.out" 2>&1 || code=$?
expect 0 "${code:-0}" "a healthy candidate swaps and stays"
expect_grep "canary is live" "$tmp/start.out" "start prints the live marker"
expect "conbus 0.0.0-beef (beef)" "$("$install_dir/conbus" --version)" "the candidate is installed"
expect "conbus 0.0.0-01d (01d)" "$("$rollback_bin" --version)" "conbus.rollback kept the replaced binary"
expect_absent "$install_dir/conbus.new" "write-beside temp is renamed, not left"
expect_absent "$home/.install-called" "rehearsal never invoked install --apply"
expect_file "$home/.conbus/canary.json" "canary.json records the window"
expect_grep '"candidate_build":"beef"' "$home/.conbus/canary.json" "canary.json names the candidate build"

# --- auto-rollback scenarios -------------------------------------------------

expect_live() { # expect the install to still be the candidate or restored old
    [ "$("$install_dir/conbus" --version)" = "conbus 0.0.0-$1 ($1)" ] || {
        echo "rehearsal: FAIL: $2" >&2
        exit 1
    }
    echo "rehearsal: ok: $2"
}
restore_candidate() { write_fake beef live "$install_dir/conbus"; }

# serving never appears: the stamp stays old -> auto rollback.
code=0
SERVING_LIMIT=1 run start --from "$tmp/stage/conbus-noserve" >"$tmp/noserve.out" 2>&1 || code=$?
expect 1 "$code" "no serving stamp -> start exits 1"
expect_live 01d "no serving stamp -> rolled back automatically"
expect_grep "rolling back" "$tmp/noserve.out" "the rollback is announced"
expect_grep "serving" "$tmp/noserve.out" "the reason names serving"

# DIFFERS: the answering daemon is another build -> auto rollback.
daemon_boot beef 4301
code=0
SERVING_LIMIT=2 run start --from "$tmp/stage/conbus-differs" >"$tmp/diff.out" 2>&1 || code=$?
expect 1 "$code" "DIFFERS -> start exits 1"
expect_live 01d "DIFFERS -> rolled back automatically"
expect_grep "different build" "$tmp/diff.out" "the reason names the build"

# a hook is not `runs` -> auto rollback.
daemon_boot beef 4302
code=0
SERVING_LIMIT=2 run start --from "$tmp/stage/conbus-hookfail" >"$tmp/hook.out" 2>&1 || code=$?
expect 1 "$code" "a hook not runs -> start exits 1"
expect_live 01d "a hook not runs -> rolled back automatically"
expect_grep "hook" "$tmp/hook.out" "the reason names the hook"

# a new drop reason after the swap -> auto rollback.
daemon_boot beef 4303
code=0
SERVING_LIMIT=2 run start --from "$tmp/stage/conbus-dropper" >"$tmp/drop.out" 2>&1 || code=$?
expect 1 "$code" "a new drop reason -> start exits 1"
expect_live 01d "a new drop reason -> rolled back automatically"
expect_grep "drop" "$tmp/drop.out" "the reason names drops"

# --- restart + verdict -------------------------------------------------------

# Planned restarts are recorded; unplanned ones are counted by verdict.
# Scenario: swap happened >7 days ago, two planned restarts, one crash.
cat >"$home/.conbus/canary.json" <<'EOF'
{"swapped":"2026-10-20T00:00:00.000Z","prev_build":"01d","candidate_build":"beef","candidate_sha256":"x","restarts":[{"ts":"2026-10-21T00:00:00.000Z","serving_ms":4000,"new_drops":0},{"ts":"2026-10-26T00:00:00.000Z","serving_ms":4100,"new_drops":0}]}
EOF
cat >"$home/.conbus/daemon.log" <<'EOF'
2026-10-20T00:00:05.000Z conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)
2026-10-21T00:00:04.000Z conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)
2026-10-26T00:00:04.000Z conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)
EOF
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-15T09:00:00.000Z","reason":"socket closed early"}
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
EOF
cat >"$home/.conbus/daemon.build" <<EOF
{"build":"beef","pid":$$,"started":"2026-10-26T00:00:00.000Z","serving":"2026-10-26T00:00:04.000Z"}
EOF
restore_candidate
# A rollback binary that answers usage identically (minus tokens).
write_fake 01d live "$rollback_bin"
# verdict: 3 starts - 1 swap - 2 planned = 0 unplanned, closed days agree.
code=0
run verdict >"$tmp/verdict.out" 2>&1 || code=$?
expect 0 "$code" "verdict exits 0 on a completed clean window"
for name in restarts unreadable drops harness-activity twins-repeated usage-parity \
    rss-budget time-budget serving-budget policy sync operator-feel; do
    grep -qE "^(PASS|FAIL|UNCHECKED) $name " "$tmp/verdict.out"
    expect 0 $? "verdict prints a $name line"
done
expect_grep '^PASS restarts' "$tmp/verdict.out" "planned restarts are not counted as unplanned"
expect_grep '^PASS usage-parity' "$tmp/verdict.out" "closed-day usage parity passes"
expect_grep '^PASS rss-budget' "$tmp/verdict.out" "rss-budget measured against CONBUS_CANARY_RSS_MB"
expect_grep '^PASS time-budget' "$tmp/verdict.out" "time budgets are measured"
expect_grep '^PASS serving-budget' "$tmp/verdict.out" "serving <= 5 s is a verdict line"
expect_grep '^UNCHECKED operator-feel' "$tmp/verdict.out" "only operator-feel stays UNCHECKED"

# A crash after the swap is still visible — the window does not move.
cat >>"$home/.conbus/daemon.log" <<'EOF'
2026-10-28T03:00:04.000Z conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)
EOF
code=0
run verdict >"$tmp/verdict-crash.out" 2>&1 || code=$?
expect 1 "$code" "a crash after the swap fails the verdict"
expect_grep '^FAIL restarts' "$tmp/verdict-crash.out" "the crash is counted as unplanned"
sed -i '' '$d' "$home/.conbus/daemon.log"

# A parity mismatch fails.
cat >"$rollback_bin" <<'EOF'
#!/bin/sh
case "$1" in
    --version) echo "conbus 0.0.0-01d (01d)" ;;
    usage) echo '{"day":"x","harness":"claude_code","different":true}' ;;
    *) echo old ;;
esac
EOF
chmod 755 "$rollback_bin"
code=0
run verdict >"$tmp/verdict-parity.out" 2>&1 || code=$?
expect 1 "$code" "a usage parity mismatch fails"
expect_grep '^FAIL usage-parity' "$tmp/verdict-parity.out" "the mismatch day is named FAIL"
write_fake 01d live "$rollback_bin"

# A planned restart's drop delta fails the same way a new reason does.
cat >"$home/.conbus/canary.json" <<'EOF'
{"swapped":"2026-10-20T00:00:00.000Z","prev_build":"01d","candidate_build":"beef","candidate_sha256":"x","restarts":[{"ts":"2026-10-21T00:00:00.000Z","serving_ms":4000,"new_drops":1}]}
EOF
code=0
run verdict >"$tmp/verdict-rdrop.out" 2>&1 || code=$?
expect 1 "$code" "a drop during a planned restart fails"
expect_grep '^FAIL drops' "$tmp/verdict-rdrop.out" "the planned-restart drop is named FAIL"

# Inside the window the verdict is "incomplete", not a pass.
cat >"$home/.conbus/canary.json" <<EOF
{"swapped":"$(date -u +%Y-%m-%dT00:00:00.000Z)","prev_build":"01d","candidate_build":"beef","candidate_sha256":"x","restarts":[]}
EOF
cat >"$home/.conbus/daemon.log" <<EOF
$(date -u +%Y-%m-%dT00:00:01.000Z) conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)
EOF
code=0
run verdict >"$tmp/verdict-open.out" 2>&1 || code=$?
if [ "$code" != 3 ]; then cat "$tmp/verdict-open.out" >&2; fi
expect 3 "$code" "an open window exits 3"
expect_grep "window incomplete" "$tmp/verdict-open.out" "it says the window is incomplete"

# restart records a planned restart into canary.json.
code=0
run restart >/dev/null 2>&1 || code=$?
expect 0 "${code:-0}" "restart records a planned restart (rehearsal)"
expect_grep '"serving_ms"' "$home/.conbus/canary.json" "the restart entry is in canary.json"

# rollback restores the original; the rollback copy is consumed.
run rollback >/dev/null
expect 0 $? "rollback"
expect "conbus 0.0.0-01d (01d)" "$("$install_dir/conbus" --version)" "rollback restored the original"
expect_absent "$rollback_bin" "conbus.rollback is consumed by the mv back"
code=0
run rollback >/dev/null 2>&1 || code=$?
expect 1 "$code" "rollback refuses when conbus.rollback is missing"
code=0
run start --from "$tmp/stage/nonexistent" >/dev/null 2>&1 || code=$?
expect 1 "$code" "start refuses a missing candidate"

echo "rehearsal: all checks passed"
