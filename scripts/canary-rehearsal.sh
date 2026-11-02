#!/bin/bash
# Rehearse canary.sh end-to-end under a throwaway HOME: a fake install dir,
# fake conbus binaries, and two guards: CONBUS_CANARY_REHEARSAL=1 +
# CONBUS_SKIP_LAUNCHCTL=1 (swap runs, launchctl and install --apply are
# skipped), and CONBUS_CANARY_TEST_REAL_HOME=<the same throwaway HOME>
# which walks the real code path — the fake's `install --apply` answers
# for the reload. Nothing here can touch the real ~/.local/bin or
# ~/.conbus — every swapping invocation uses a mktemp HOME, and the one
# guard test that points HOME at the passwd home must be refused before
# anything moves.
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

DAY=86400

iso_ago() { # iso_ago <seconds> — a UTC stamp in the shape the logs use
    perl -MPOSIX=strftime -e 'print strftime("%Y-%m-%dT%H:%M:%S.000Z", gmtime(time - $ARGV[0]))' "$1"
}

iso_after() { # iso_after <iso> <seconds> — seconds may be negative
    perl -MTime::Local -MPOSIX=strftime -e '
        my ($y, $mo, $d, $h, $mi, $s) = $ARGV[0] =~ /^(\d+)-(\d+)-(\d+)T(\d+):(\d+):(\d+)/;
        print strftime("%Y-%m-%dT%H:%M:%S.000Z", gmtime(timegm($s, $mi, $h, $d, $mo - 1, $y) + $ARGV[1]))' "$1" "$2"
}

# A row per harness per day, from <first> through today. Relative to today, so
# no scenario runs out of rows the way fixed dates eventually would.
usage_rows() { # usage_rows <first-iso> <harness>...
    perl -MTime::Local -MPOSIX=strftime -e '
        my ($y, $mo, $d) = shift(@ARGV) =~ /^(\d+)-(\d+)-(\d+)/;
        my $today = strftime("%Y-%m-%d", gmtime(time));
        for (my $t = timegm(0, 0, 0, $d, $mo - 1, $y); ; $t += 86400) {
            my $day = strftime("%Y-%m-%d", gmtime($t));
            last if $day gt $today;
            print qq({"day":"$day","harness":"$_","turns":3,"tool_calls":40,"tool_denials":0,"repeated":0,"recovered":0,"tokens_input":1}\n) for @ARGV;
        }' "$@"
}

# A fake conbus. $2 picks the doctor persona the scenario needs:
#   silent    — socket not answering (the daemon is down or absent)
#   live      — socket listening, matching daemon build, hooks all `runs`
#   differs   — the answering daemon is another build
#   hookfail  — a hooks line that is not `runs`
#   dropper   — like live, but drops a new reason into drops.jsonl
#   swapdrop  — its `install --apply` drops the expected swap-window
#               protocol refusal, like a hook firing mid-restart
#   sessions  — its `install --apply` drops a sessionless refusal
#   notokens  — usage rows without the tokens_* keys (the 0.2.8 shape)
#   stuck     — doctor never answers: a wedged vendor probe, like the
#               unbounded ones a 0.2.x doctor runs
#   stuckboot — doctor stamps the daemon build, then never answers: the
#               rollback-verify path can pass the stamp check and still
#               hang on the agreeing-doctor check
#   chatty    — like live, with far more output after the socket line than a pipe
#               holds, so a reader that leaves at its first match costs the
#               writer a SIGPIPE every time instead of once in a while
#   rotating  — like live, and it names drops.1.jsonl the way a build that
#               rotates its drop log does
#   elided    — its `install --apply` writes `tool bodies elided, not sealed` and
#               `the log refused writes for a while`, like a spool drained at boot
#   flap      — doctor sleeps on its first call, then answers fast: a
#               single-shot time budget flakes where a median does not
#   polfail   — `policy check` exits 1: a file that cannot be a policy
write_fake() {
    tag=$1
    persona=${2:-silent}
    path=$3
    install_body='touch "$HOME/.install-called"'
    case $persona in
        old) install_body='touch "$HOME/.install-called"; printf "{\"build\":\"'"$tag"'\",\"pid\":$$,\"started\":\"2026-11-01T10:00:00.000Z\",\"serving\":\"2026-11-01T10:00:04.000Z\"}\n" > "$HOME/.conbus/daemon.build"'
            doctor='printf "{\"build\":\"'"$tag"'\",\"pid\":$$,\"started\":\"2026-11-01T10:00:00.000Z\",\"serving\":\"2026-11-01T10:00:04.000Z\"}\n" > "$HOME/.conbus/daemon.build"; echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid $$ since 2026-11-01T10:00:00.000Z"; echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"; echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"; echo "hooks     claude_code  runs — x"; echo "events    claude_code  13/13 registered"' ;;
        diskwarn) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "disk      log 1.0 MB — 0.1 GB free of 2.0 GB (5.0%) — WARNING: under 10% free"; echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"' ;;
        silent) doctor='echo "socket    $HOME/.conbus/conbus.sock — not answering"; echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"; echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"' ;;
        live) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"; echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"; echo "hooks     claude_code  runs — $HOME/.local/bin/conbus hook"; echo "events    claude_code  13/13 registered"' ;;
        differs) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build aaaa pid 9999 since 2026-11-01T10:00:00.000Z — DIFFERS from this binary ('"$tag"'); restart the daemon"' ;;
        hookfail) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     codex        MISSING — /h hooks.json has no conbus entry"' ;;
        dropper) doctor='echo "{\"ts\":\"2030-01-01T00:00:00.000Z\",\"reason\":\"post-swap fault\"}" >> "$HOME/.conbus/drops.jsonl"; echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     claude_code  runs — x"' ;;
        swapdrop) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     claude_code  runs — x"'
            install_body='touch "$HOME/.install-called"; printf "{\"ts\":\"%s\",\"reason\":\"the daemon refused the record: this daemon speaks protocol v5; the hook speaks v6. Restart the daemon.\"}\n" "$(perl -MTime::HiRes=gettimeofday -MPOSIX=strftime -e "(\$s,\$u)=gettimeofday; printf \"%s.%03dZ\", strftime(\"%Y-%m-%dT%H:%M:%S\", gmtime(\$s)), int(\$u/1000)")" >> "$HOME/.conbus/drops.jsonl"' ;;
        sessions) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     claude_code  runs — x"'
            install_body='touch "$HOME/.install-called"; printf "{\"ts\":\"%s\",\"reason\":\"the hook payload names no session; an empty id would merge unrelated sessions into one conversation\"}\n" "$(perl -MTime::HiRes=gettimeofday -MPOSIX=strftime -e "(\$s,\$u)=gettimeofday; printf \"%s.%03dZ\", strftime(\"%Y-%m-%dT%H:%M:%S\", gmtime(\$s)), int(\$u/1000)")" >> "$HOME/.conbus/drops.jsonl"' ;;
        rotating) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"; echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"; echo "hooks     claude_code  runs — x"' ;;
        elided) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     claude_code  runs — x"'
            install_body='touch "$HOME/.install-called"; now=$(perl -MTime::HiRes=gettimeofday -MPOSIX=strftime -e "(\$s,\$u)=gettimeofday; printf \"%s.%03dZ\", strftime(\"%Y-%m-%dT%H:%M:%S\", gmtime(\$s)), int(\$u/1000)"); printf "{\"detail\":\"7 tool event(s)\",\"reason\":\"tool bodies elided, not sealed\",\"ts\":\"%s\"}\n{\"reason\":\"the log refused writes for a while\",\"ts\":\"%s\"}\n" "$now" "$now" >> "$HOME/.conbus/drops.jsonl"' ;;
        knowndrop) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     claude_code  runs — x"'
            install_body='touch "$HOME/.install-called"; printf "{\"ts\":\"%s\",\"reason\":\"socket closed early\"}\n" "$(perl -MTime::HiRes=gettimeofday -MPOSIX=strftime -e "(\$s,\$u)=gettimeofday; printf \"%s.%03dZ\", strftime(\"%Y-%m-%dT%H:%M:%S\", gmtime(\$s)), int(\$u/1000)")" >> "$HOME/.conbus/drops.jsonl"' ;;
        notokens) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "hooks     claude_code  runs — x"' ;;
        stuck) doctor='sleep 3600' ;;
        stuckboot) doctor='printf "{\"build\":\"'"$tag"'\",\"pid\":$$,\"started\":\"2026-11-01T10:00:00.000Z\",\"serving\":\"2026-11-01T10:00:04.000Z\"}\n" > "$HOME/.conbus/daemon.build"; sleep 3600' ;;
        chatty) doctor='echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "disk      log 1.0 MB — 1.0 GB free of 2.0 GB (50.0%)"; echo "hooks     claude_code  runs — x"; perl -e "print qq(note\n) x 60000"' ;;
        flap) doctor='n=$(cat "$HOME/.flap" 2>/dev/null || echo 0); echo $((n + 1)) > "$HOME/.flap"; [ "$n" = 0 ] && sleep 3; echo "socket    $HOME/.conbus/conbus.sock — listening"; echo "daemon    build '"$tag"' pid 9999 since 2026-11-01T10:00:00.000Z"; echo "log       5 record(s), 1 conversation(s), 0 unreadable line(s)"' ;;
    esac
    policy_body=':'
    [ "$persona" = polfail ] && policy_body='exit 1'
    # Both binaries read the rows a scenario wrote to $HOME/.usage-rows, as two
    # builds read one log; the older one without the tokens_* keys. .usage-fail
    # fails every read, .usage-fail-day only that day's.
    usage_filter=cat
    if [ "$persona" = notokens ]; then
        usage_filter="sed -E 's/\"tokens_[a-z_]*\":[0-9]*,?//g; s/,}/}/g'"
    fi
    rotates_note=""
    [ "$persona" = rotating ] && rotates_note="# renames a full log to drops.1.jsonl"
    cat >"$path" <<EOF
#!/bin/sh
$rotates_note
case "\$1" in doctor|quota|status) echo "\${CONBUS_AGY_BIN:-unset}" >>"$tmp/agy-bin.seen" ;; esac
case "\$1" in
    --version) echo "conbus 0.0.0-$tag ($tag)" ;;
    doctor) $doctor ;;
    log) echo "cache     0 closed segment(s): 0 built, 0 already valid" ;;
    usage)
        [ -f "\$HOME/.usage-fail" ] && { echo "a segment could not be read" >&2; exit 1; }
        case "\$2 \$3" in
            "--json --day")
                [ "\$4" = "\$(cat "\$HOME/.usage-fail-day" 2>/dev/null)" ] &&
                    { echo "a segment could not be read" >&2; exit 1; }
                grep "\"day\":\"\$4\"" "\$HOME/.usage-rows" 2>/dev/null | $usage_filter
                ;;
            "--json --since")
                case "\$4" in
                    *T*) [ -f "\$HOME/.usage-window" ] && { cat "\$HOME/.usage-window"; exit 0; } ;;
                esac
                awk -v s="\${4%%T*}" 'match(\$0, /"day":"[^"]*"/) && substr(\$0, RSTART + 7, 10) >= s' \
                    "\$HOME/.usage-rows" 2>/dev/null | $usage_filter
                ;;
        esac
        exit 0
        ;;
    policy) $policy_body ;;
    install) $install_body ;;
    *) echo "fake-$tag: \$*" ;;
esac
EOF
    chmod 755 "$path"
}

# --- fixtures ---------------------------------------------------------------

# codesign for the fakes on the real path, which are not signed. It writes a
# line at a time like the real one, so a reader that leaves early costs it a
# SIGPIPE.
write_codesign() { # write_codesign <dir> <--verify exit> <flags>
    mkdir -p "$1"
    cat >"$1/codesign" <<EOF
#!/bin/sh
[ "\$1" = --verify ] && exit $2
echo 'CodeDirectory v=20500 size=7701 flags=$3 hashes=230+7 location=embedded' >&2
sleep 0.1
echo 'TeamIdentifier=ABCDE12345' >&2
EOF
    chmod 755 "$1/codesign"
}
write_codesign "$tmp/signed" 0 '0x10000(runtime)'
write_codesign "$tmp/no-runtime" 0 '0x0(none)'
write_codesign "$tmp/unverified" 1 '0x10000(runtime)'

# `agy models` as the real path asks it: with no login, and with one.
printf '#!/bin/sh\nexit 1\n' >"$tmp/agy-no-login"
printf '#!/bin/sh\nexit 0\n' >"$tmp/agy-signed-in"
chmod 755 "$tmp/agy-no-login" "$tmp/agy-signed-in"

write_fake 01d old "$install_dir/conbus"          # the installed "0.2.8"
write_fake beef live "$tmp/stage/conbus-good"        # a healthy candidate
write_fake beef differs "$tmp/stage/conbus-differs"
write_fake beef hookfail "$tmp/stage/conbus-hookfail"
write_fake beef dropper "$tmp/stage/conbus-dropper"
write_fake beef silent "$tmp/stage/conbus-noserve"   # daemon never serves
write_fake beef swapdrop "$tmp/stage/conbus-swapdrop" # swap-window drop at install
write_fake beef sessions "$tmp/stage/conbus-sessions" # sessionless drop at install
write_fake beef stuck "$tmp/stage/conbus-stuck"       # doctor never answers

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

# The read budgets are wide here: a scenario is not about this machine's load
# unless it sets one itself.
run() {
    env -u CONBUS_HOME -u XDG_STATE_HOME \
        HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 CONBUS_CANARY_REHEARSAL=1 \
        CONBUS_CANARY_TAIL_MS=${TAIL_MS:-20000} CONBUS_CANARY_USAGE_MS=${USAGE_MS:-20000} \
        CONBUS_CANARY_DOCTOR_MS=${DOCTOR_MS:-20000} \
        CONBUS_CANARY_SERVING_LIMIT=${SERVING_LIMIT:-30} \
        bash "$canary" "$@"
}

run_norehearsal() {
    env -u CONBUS_HOME -u XDG_STATE_HOME \
        HOME="$home" CONBUS_SKIP_LAUNCHCTL=1 \
        CONBUS_CANARY_SERVING_LIMIT=${SERVING_LIMIT:-30} \
        bash "$canary" "$@"
}

# The seam: a throwaway HOME that names itself CONBUS_CANARY_TEST_REAL_HOME
# walks the real code path — swap runs, the fake's `install --apply`
# answers for the reload. No SKIP, no REHEARSAL.
run_real() {
    env -u CONBUS_HOME -u XDG_STATE_HOME -u CONBUS_SKIP_LAUNCHCTL \
        -u CONBUS_CANARY_REHEARSAL \
        HOME="$home" CONBUS_CANARY_TEST_REAL_HOME="$home" \
        CONBUS_AGY_BIN="${real_path_agy:-$tmp/agy-no-login}" \
        CONBUS_CANARY_SERVING_LIMIT=${SERVING_LIMIT:-30} \
        PATH="$tmp/${real_path_signing:-signed}:$PATH" \
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
    if ! LC_ALL=C grep -aq "$1" "$2"; then
        echo "rehearsal: FAIL: $3 — no '$1' in $2:" >&2
        cat "$2" >&2
        exit 1
    fi
    echo "rehearsal: ok: $3"
}

write_stamp() { # write_stamp <build> <pid>
    printf '{"build":"%s","pid":%s,"started":"%s","serving":"%s","core_dumps_disabled":true,"memory_locked":true}\n' \
        "$1" "$2" "$(date -u +%Y-%m-%dT%H:%M:%S.000Z)" "$(date -u +%Y-%m-%dT%H:%M:%S.002Z)" \
        >"$home/.conbus/daemon.build"
}

# Simulate the daemon coming up after install --apply. First writes the OLD
# daemon's stamp synchronously (pid-1), which `start` reads as old_stamp_pid.
# The new stamp lands once the script has swapped the binary or run install
# --apply, and both come after that read. A fixed delay lost that race to a
# slow preflight: wait_for_serving then waited for a change that had already
# happened, and scenarios failed on a busy machine.
daemon_boot() { # daemon_boot <build> <pid>
    if [ -n "${boot_job:-}" ]; then
        kill "$boot_job" 2>/dev/null || true
        wait "$boot_job" 2>/dev/null || true
    fi
    write_stamp 01d "$(($2 - 1))"
    rm -f "$home/.install-called"
    installed=$(ls -i "$install_dir/conbus" 2>/dev/null || true)
    (
        n=0
        while [ "$(ls -i "$install_dir/conbus" 2>/dev/null || true)" = "$installed" ] &&
            [ ! -e "$home/.install-called" ]; do
            n=$((n + 1))
            [ "$n" -lt 1200 ] || exit 0
            sleep 0.05
        done
        [ -d "$home/.conbus" ] || exit 0
        write_stamp "$1" "$2"
    ) &
    boot_job=$!
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

# The candidate's signature, which only the real path checks. A rehearsal that
# skips it let a broken check reach the first real start.
if [ "$(uname -s)" = Darwin ]; then
    code=0
    run_real preflight --from "$tmp/stage/conbus-good" --evidence "$tmp/evidence" \
        >"$tmp/preflight-signed.out" 2>&1 || code=$?
    expect 0 "$code" "preflight on the real path passes a signed candidate"
    expect_grep '^PASS signature' "$tmp/preflight-signed.out" "the signature is checked"
    expect_grep '^PASS hardened-runtime' "$tmp/preflight-signed.out" "a hardened runtime is found"

    code=0
    real_path_signing=no-runtime run_real preflight --from "$tmp/stage/conbus-good" \
        --evidence "$tmp/evidence" >"$tmp/preflight-noruntime.out" 2>&1 || code=$?
    expect 1 "$code" "preflight fails a candidate with no hardened runtime"
    expect_grep '^FAIL hardened-runtime' "$tmp/preflight-noruntime.out" "the missing runtime is named"

    code=0
    real_path_signing=unverified run_real preflight --from "$tmp/stage/conbus-good" \
        --evidence "$tmp/evidence" >"$tmp/preflight-unverified.out" 2>&1 || code=$?
    expect 1 "$code" "preflight fails a candidate that does not verify"
    expect_grep '^FAIL signature' "$tmp/preflight-unverified.out" "the bad signature is named"
fi
expect_file "$home/Library/LaunchAgents/sh.conbus.daemon.plist.bak" \
    "preflight backed the plist up beside itself"

# A DIFFERS from the installed doctor fails preflight.
write_fake bad differs "$install_dir/conbus"
code=0
run preflight >/dev/null 2>&1 || code=$?
expect 1 "$code" "preflight fails on DIFFERS"
write_fake bad diskwarn "$install_dir/conbus"
code=0
run preflight >"$tmp/preflight-disk.out" 2>&1 || code=$?
expect 1 "$code" "a disk WARNING fails preflight"
expect_grep '^FAIL disk' "$tmp/preflight-disk.out" "the disk line is named"
code=0
CONBUS_CANARY_DISK_WARN_OK=1 run preflight >"$tmp/preflight-disk-ok.out" 2>&1 || code=$?
expect 0 "$code" "CONBUS_CANARY_DISK_WARN_OK=1 accepts the warning"
expect_grep '^WARN disk' "$tmp/preflight-disk-ok.out" "the disk line downgrades to WARN"
expect_grep 'CONBUS_CANARY_DISK_WARN_OK' "$tmp/preflight-disk-ok.out" "the acceptance is named"

# A wedged doctor — a vendor probe that never answers — must fail the
# preflight inside CONBUS_CANARY_DOCTOR_LIMIT, not hang it.
write_fake bad stuck "$install_dir/conbus"
code=0
CONBUS_CANARY_DOCTOR_LIMIT=2 run preflight >"$tmp/preflight-stuck.out" 2>&1 || code=$?
expect 1 "$code" "a wedged doctor fails preflight instead of hanging it"
expect_grep 'doctor' "$tmp/preflight-stuck.out" "the wedged doctor is named"
write_fake 01d old "$install_dir/conbus"

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
expect 2 "$code" "start without rehearsal env refuses (exit 2)"
code=0
run_norehearsal rollback >/dev/null 2>&1 || code=$?
expect 2 "$code" "rollback without rehearsal env refuses (exit 2)"
code=0
run_norehearsal restart >/dev/null 2>&1 || code=$?
expect 2 "$code" "restart without rehearsal env refuses (exit 2)"

# --live is gone: refused as an unknown argument.
code=0
run start --live --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 2 "$code" "--live is refused (exit 2)"

# A rehearsal under the real passwd HOME is refused BEFORE anything moves —
# this is the guard that makes a stray REHEARSAL flag safe. The refusal must
# come before preflight or the file swap, and the live binary must not move.
# Resolve the passwd home the same way canary.sh does — `cd ~` with HOME
# unset answers from getpwuid; plain `cd` needs HOME itself.
real_home=$(env -u HOME bash -c 'cd ~ && pwd -P' 2>/dev/null)
live_bin=$real_home/.local/bin/conbus
live_sha=""
[ -f "$live_bin" ] && live_sha=$(shasum -a 256 "$live_bin" | awk '{print $1}')
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$real_home" CONBUS_CANARY_REHEARSAL=1 \
    bash "$canary" start --from "$tmp/stage/conbus-good" >"$tmp/realhome.out" 2>&1 || code=$?
if [ "${code:-0}" != 2 ]; then
    say "diagnostic: real_home='$real_home' pw='$(env -u HOME bash -c 'cd ~ && pwd -P' 2>/dev/null)'"
    sed 's/^/  canary: /' "$tmp/realhome.out" | head -8
fi
expect 2 "$code" "REHEARSAL under the real HOME refuses (exit 2)"
if [ -n "$live_sha" ]; then
    expect "$live_sha" "$(shasum -a 256 "$live_bin" | awk '{print $1}')" \
        "the live binary is untouched"
fi
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME HOME="$real_home" \
    CONBUS_CANARY_REHEARSAL=1 CONBUS_SKIP_LAUNCHCTL=1 \
    bash "$canary" start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 2 "$code" "REHEARSAL + SKIP under the real HOME still refuses (exit 2)"
if [ -n "$live_sha" ]; then
    expect "$live_sha" "$(shasum -a 256 "$live_bin" | awk '{print $1}')" \
        "no half deploy — the binary is still untouched"
fi

# The test seam only accepts a throwaway HOME that names itself.
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME -u CONBUS_SKIP_LAUNCHCTL \
    HOME="$home" CONBUS_CANARY_TEST_REAL_HOME="$real_home" \
    bash "$canary" start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 2 "$code" "seam pointing at the passwd home refuses (exit 2)"
other_tmp=$(mktemp -d "$tmp/other.XXXXXX")
code=0
env -u CONBUS_HOME -u XDG_STATE_HOME -u CONBUS_SKIP_LAUNCHCTL \
    HOME="$home" CONBUS_CANARY_TEST_REAL_HOME="$other_tmp" \
    bash "$canary" start --from "$tmp/stage/conbus-good" >/dev/null 2>&1 || code=$?
expect 2 "$code" "seam naming a different dir refuses (exit 2)"

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

# A healthy doctor with a lot to say. `echo "$out" | grep -q listening` leaves at the
# first match, echo takes a SIGPIPE, pipefail fails the pipeline, and a daemon that
# said `listening` was rolled back as not answering. Seen once in about fifteen
# runs with ordinary output; with this much it happens every time.
write_fake beef chatty "$tmp/stage/conbus-chatty"
write_fake 01d old "$install_dir/conbus"
rm -f "$rollback_bin" "$home/.conbus/canary.json"
daemon_boot beef 4249
code=0
run start --from "$tmp/stage/conbus-chatty" >"$tmp/start-chatty.out" 2>&1 || code=$?
if [ "$code" != 0 ]; then tail -8 "$tmp/start-chatty.out" >&2; fi
expect 0 "$code" "a doctor with a lot to say does not roll a healthy swap back"
# Preflight tells the operator to rotate a capped log aside, so no drops.jsonl
# at all is a normal way to arrive here. It used to end start with exit 1 and
# not a word, on the last `[ -f ]` of a loop under pipefail.
write_fake 01d old "$install_dir/conbus"
rm -f "$rollback_bin" "$home/.conbus/canary.json" "$home/.conbus/drops.jsonl" "$home/.conbus/drops.1.jsonl"
daemon_boot beef 4250
code=0
run start --from "$tmp/stage/conbus-good" >"$tmp/start-nodrops.out" 2>&1 || code=$?
if [ "$code" != 0 ]; then cat "$tmp/start-nodrops.out" >&2; fi
expect 0 "$code" "start proceeds with no drop log"
expect_grep "canary is live" "$tmp/start-nodrops.out" "and reaches its live marker"
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
EOF

# --- the real path through the test seam -------------------------------------

# The same swap through the real code path: install --apply runs (the fake
# answers for it) and its marker proves the branch was real.
write_fake 01d old "$install_dir/conbus"
rm -f "$rollback_bin" "$home/.conbus/canary.json" "$home/.install-called"
daemon_boot beef 4244
code=0
run_real start --from "$tmp/stage/conbus-good" >"$tmp/start-real.out" 2>&1 || code=$?
expect 0 "${code:-0}" "the real path swaps through the test seam"
expect_file "$home/.install-called" "the real path invoked install --apply"
expect "conbus 0.0.0-beef (beef)" "$("$install_dir/conbus" --version)" "the candidate is installed (real path)"

# F1 — the swap-window drop a normal cutover produces: the new hook (v6)
# reaches the old daemon (v5), is refused, and cannot spool because the old
# stamp names no retention. Counted, recorded, NOT rolled back.
write_fake 01d old "$install_dir/conbus"
rm -f "$home/.install-called" "$home/.conbus/canary.json"
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
EOF
daemon_boot beef 4245
code=0
run_real start --from "$tmp/stage/conbus-swapdrop" >"$tmp/start-swapdrop.out" 2>&1 || code=$?
expect 0 "${code:-0}" "a swap-window protocol refusal does not roll back"
expect_grep "swap-window drop" "$tmp/start-swapdrop.out" "the expected drop is announced"
expect_grep '"swap_window_drops":1' "$home/.conbus/canary.json" \
    "canary.json records swap_window_drops"

# A spool drained at the first boot elides its tool bodies, and an outage the daemon
# outlasted says so once writes work again. The records landed; neither line may
# undo a healthy swap.
write_fake beef elided "$tmp/stage/conbus-elided"
write_fake 01d old "$install_dir/conbus"
rm -f "$rollback_bin" "$home/.install-called" "$home/.conbus/canary.json"
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
EOF
daemon_boot beef 4251
code=0
run_real start --from "$tmp/stage/conbus-elided" >"$tmp/start-elided.out" 2>&1 || code=$?
if [ "$code" != 0 ]; then cat "$tmp/start-elided.out" >&2; fi
expect 0 "$code" "lines that lost no record do not roll a swap back"
expect "conbus 0.0.0-beef (beef)" "$("$install_dir/conbus" --version)" "the candidate stays"

# A drop reason that is NOT a swap artifact still rolls back under the
# real path — the tolerance is narrow by design.
write_fake 01d old "$install_dir/conbus"
rm -f "$rollback_bin" "$home/.install-called" "$home/.conbus/canary.json"
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
EOF
daemon_boot beef 4246
code=0
run_real start --from "$tmp/stage/conbus-dropper" >"$tmp/start-realdrop.out" 2>&1 || code=$?
expect 1 "$code" "a non-swap drop reason still rolls back (real path)"
expect "conbus 0.0.0-01d (01d)" "$("$install_dir/conbus" --version)" \
    "the real-path rollback restored the previous binary"
expect_absent "$rollback_bin" "the rollback copy was consumed"

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
expect_grep "rollback verified" "$tmp/noserve.out" "the old build came back and is verified"

# DIFFERS: the answering daemon is another build -> auto rollback.
daemon_boot beef 4301
code=0
SERVING_LIMIT=2 run start --from "$tmp/stage/conbus-differs" >"$tmp/diff.out" 2>&1 || code=$?
expect 1 "$code" "DIFFERS -> start exits 1"
expect_live 01d "DIFFERS -> rolled back automatically"
expect_grep "different build" "$tmp/diff.out" "the reason names the build"
expect_grep "rollback verified" "$tmp/diff.out" "the old build came back and is verified"

# a hook is not `runs` -> auto rollback.
daemon_boot beef 4302
code=0
SERVING_LIMIT=2 run start --from "$tmp/stage/conbus-hookfail" >"$tmp/hook.out" 2>&1 || code=$?
expect 1 "$code" "a hook not runs -> start exits 1"
expect_live 01d "a hook not runs -> rolled back automatically"
expect_grep "hook" "$tmp/hook.out" "the reason names the hook"
expect_grep "rollback verified" "$tmp/hook.out" "the old build came back and is verified"

# a new drop reason after the swap -> auto rollback. (drops reset to a clean
# baseline — a reason already in the log is 'known' and tolerated, so the
# scenario must present a genuinely new one.)
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
EOF
daemon_boot beef 4303
code=0
SERVING_LIMIT=2 run start --from "$tmp/stage/conbus-dropper" >"$tmp/drop.out" 2>&1 || code=$?
expect 1 "$code" "a new drop reason -> start exits 1"
expect_live 01d "a new drop reason -> rolled back automatically"
expect_grep "drop" "$tmp/drop.out" "the reason names drops"
expect_grep "rollback verified" "$tmp/drop.out" "the old build came back and is verified"

# (b) a post-swap failure whose rollback never comes back: the restored
# binary cannot boot a daemon, so the stamp keeps the candidate's pid and
# build. exit 3, not the ordinary 1.
write_fake 01d silent "$rollback_bin"
restore_candidate
code=0
SERVING_LIMIT=1 run start --from "$tmp/stage/conbus-dropper" >"$tmp/norb.out" 2>&1 || code=$?
expect 3 "$code" "unverified rollback -> start exits 3"
expect_grep "ROLLBACK NOT VERIFIED" "$tmp/norb.out" "the script says the rollback was not verified"
expect_live 01d "the rollback binary was still moved back"
expect_grep 'rolled_back' "$home/.conbus/canary.json" "an unverified rollback still records rolled_back in canary.json"
rm -f "$rollback_bin"

# (c) the rollback subcommand, verified and not.
write_fake 01d old "$rollback_bin"
write_stamp beef 4401
code=0
run rollback >"$tmp/rb-ok.out" 2>&1 || code=$?
expect 0 "$code" "rollback exits 0 when the old build is verified"
expect_grep "rollback verified" "$tmp/rb-ok.out" "the verification is announced"
write_fake 01d silent "$rollback_bin"
write_stamp beef 4402
code=0
SERVING_LIMIT=1 run rollback >"$tmp/rb-bad.out" 2>&1 || code=$?
expect 3 "$code" "rollback exits 3 when the old build never serves a stamp"
expect_grep "ROLLBACK NOT VERIFIED" "$tmp/rb-bad.out" "the failure is announced"
rm -f "$rollback_bin"

# A wedged doctor after the swap: the candidate's doctor never answers, so
# the post-swap check hits CONBUS_CANARY_DOCTOR_LIMIT, fails, and rolls
# back — the swap cannot hang on a vendor probe.
write_fake 01d old "$install_dir/conbus"
rm -f "$home/.install-called" "$home/.conbus/canary.json"
daemon_boot beef 4403
code=0
CONBUS_CANARY_DOCTOR_LIMIT=2 SERVING_LIMIT=2 run_real start \
    --from "$tmp/stage/conbus-stuck" >"$tmp/stuck.out" 2>&1 || code=$?
expect 1 "$code" "a wedged doctor -> start exits 1 (rolled back, not a hang)"
expect_live 01d "a wedged doctor -> rolled back automatically"
expect_grep "did not answer" "$tmp/stuck.out" "the doctor cap is named"
expect_grep "rollback verified" "$tmp/stuck.out" "the old build came back and is verified"

# The same wedge on the rollback path: the rehearsal kick writes the old
# build's stamp before the doctor hangs, so the stamp check passes and the
# verifying call is the one that hits the cap -> ROLLBACK NOT VERIFIED.
cat >"$home/.conbus/drops.jsonl" <<'EOF'
{"ts":"2026-10-30T09:00:00.000Z","reason":"socket closed early"}
EOF
write_fake 01d stuckboot "$rollback_bin"
restore_candidate
daemon_boot beef 4404
code=0
CONBUS_CANARY_DOCTOR_LIMIT=2 SERVING_LIMIT=2 run start \
    --from "$tmp/stage/conbus-dropper" >"$tmp/stuckrb.out" 2>&1 || code=$?
expect 3 "$code" "a capped rollback-verify doctor -> exit 3"
expect_grep "ROLLBACK NOT VERIFIED" "$tmp/stuckrb.out" "the cap-hit verification says NOT VERIFIED"
rm -f "$rollback_bin"

# --- restart + verdict -------------------------------------------------------

# A complete, healthy window: everything verdict reads, dated around <swapped>
# (default 7 days and an hour ago, so the window is over). A scenario then
# breaks the one thing it is about.
verdict_fixture() { # verdict_fixture [swapped-iso] [restarts-json]
    fx_swapped=${1:-$(iso_ago $((7 * DAY + 3600)))}
    fx_restarts=${2:-[]}
    # A boot job left from a start scenario would write its stamp over this one,
    # and only under load, when it is still waiting for its turn.
    if [ -n "${boot_job:-}" ]; then
        kill "$boot_job" 2>/dev/null || true
        wait "$boot_job" 2>/dev/null || true
        boot_job=""
    fi
    rm -f "$home/.conbus/drops.1.jsonl" "$home/.conbus/sync.json" \
        "$home/.conbus/policy.json" "$home/.usage-fail" "$home/.usage-fail-day" "$home/.usage-window"
    write_fake beef live "$install_dir/conbus"
    write_fake 01d notokens "$rollback_bin"
    printf '{"swapped":"%s","prev_build":"01d","candidate_build":"beef","candidate_sha256":"x","restarts":%s}\n' \
        "$fx_swapped" "$fx_restarts" >"$home/.conbus/canary.json"
    printf '%s conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)\n' \
        "$(iso_after "$fx_swapped" 4)" >"$home/.conbus/daemon.log"
    printf '{"build":"beef","pid":%s,"started":"%s","serving":"%s","core_dumps_disabled":true,"memory_locked":true}\n' \
        "$$" "$(iso_after "$fx_swapped" 1)" "$(iso_after "$fx_swapped" 4)" >"$home/.conbus/daemon.build"
    printf '{"ts":"%s","reason":"socket closed early"}\n' \
        "$(iso_after "$fx_swapped" -$((5 * DAY)))" >"$home/.conbus/drops.jsonl"
    usage_rows "$(iso_after "$fx_swapped" -$((14 * DAY)))" claude_code codex >"$home/.usage-rows"
}

expect_no_grep() { # expect_no_grep <pattern> <file> <what>
    if grep -q "$1" "$2"; then
        echo "rehearsal: FAIL: $3 — '$1' in $2:" >&2
        cat "$2" >&2
        exit 1
    fi
    echo "rehearsal: ok: $3"
}

# Planned restarts are recorded; unplanned ones are counted by verdict.
# Scenario: swap happened >7 days ago, two planned restarts, one crash.
swapped=$(iso_ago $((9 * DAY)))
restart_1=$(iso_after "$swapped" "$DAY")
restart_2=$(iso_after "$swapped" $((6 * DAY)))
verdict_fixture "$swapped" "[{\"ts\":\"$restart_1\",\"serving_ms\":4000,\"new_drops\":0},{\"ts\":\"$restart_2\",\"serving_ms\":4100,\"new_drops\":0}]"
cat >>"$home/.conbus/daemon.log" <<EOF
$(iso_after "$restart_1" 4) conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)
$(iso_after "$restart_2" 4) conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)
EOF
planned_log=$(cat "$home/.conbus/daemon.log")
# A rollback binary that answers usage identically minus the tokens_*
# keys — parity only passes when the sed strip actually removes them.
# verdict: 3 starts - 1 swap - 2 planned = 0 unplanned, closed days agree.
code=0
run verdict >"$tmp/verdict.out" 2>&1 || code=$?
if [ "$code" != 0 ]; then cat "$tmp/verdict.out" >&2; fi
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
expect_grep 'verdict: PASS' "$tmp/verdict.out" "a clean window says PASS"
expect_grep 'operator-feel is still yours' "$tmp/verdict.out" "and says what a PASS leaves open"

# A crash after the swap is still visible — the window does not move.
printf '%s conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)\n' \
    "$(iso_after "$swapped" $((8 * DAY)))" >>"$home/.conbus/daemon.log"
code=0
run verdict >"$tmp/verdict-crash.out" 2>&1 || code=$?
expect 1 "$code" "a crash after the swap fails the verdict"
expect_grep '^FAIL restarts' "$tmp/verdict-crash.out" "the crash is counted as unplanned"
printf '%s\n' "$planned_log" >"$home/.conbus/daemon.log"

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
write_fake 01d notokens "$rollback_bin"

# F1 verdict side: a swap-window reason is exempt only inside 60s after the
# swap. swapped=now-30s + a refusal at +20s -> exempt (exit 3, window open);
# the same reason at +90s -> FAIL drops.
verdict_fixture "$(iso_ago 30)"
printf '{"ts":"%s","reason":"the daemon refused the record: this daemon speaks protocol v5; the hook speaks v6. Restart the daemon."}\n' \
    "$(iso_ago 10)" >>"$home/.conbus/drops.jsonl"
code=0
run verdict >"$tmp/verdict-swap60.out" 2>&1 || code=$?
expect 3 "$code" "a swap-window refusal inside 60s is exempt (window still open)"
expect_grep 'expected swap-window' "$tmp/verdict-swap60.out" "the exempted count is shown"
# Same refusal >60s after the swap: keep the window open (swapped ~2min
# ago) with the drop at swapped+90s — past the exemption.
verdict_fixture "$(iso_ago 120)"
printf '{"ts":"%s","reason":"the daemon refused the record: this daemon speaks protocol v5; the hook speaks v6. Restart the daemon."}\n' \
    "$(iso_ago 30)" >>"$home/.conbus/drops.jsonl"
code=0
run verdict >"$tmp/verdict-swap90.out" 2>&1 || code=$?
expect 1 "$code" "the same refusal >60s after the swap FAILS"
expect_grep '^FAIL drops' "$tmp/verdict-swap90.out" "the persisting refusal is named"

# F4 — the window boundary in UTC: swapped 6d23h ago is still open (exit 3),
# 7d1h ago is complete (exit 0).
verdict_fixture "$(iso_ago $((6 * DAY + 23 * 3600)))"
code=0
run verdict >"$tmp/verdict-6d.out" 2>&1 || code=$?
expect 3 "$code" "a 6d23h window is still incomplete (exit 3)"
verdict_fixture
code=0
run verdict >"$tmp/verdict-7d.out" 2>&1 || code=$?
expect 0 "$code" "a 7d1h window completes (exit 0)"

# C6 — verdict budgets honor CONBUS_CANARY_* overrides.
verdict_fixture
code=0
DOCTOR_MS=1 run verdict >"$tmp/verdict-budget.out" 2>&1 || code=$?
expect 1 "$code" "a doctor over its env budget fails the verdict"
expect_grep '^FAIL time-budget.*doctor' "$tmp/verdict-budget.out" "the over-budget item is named"
expect_grep '^FAIL time-budget.*load ' "$tmp/verdict-budget.out" "with the load it was measured under"
code=0
TAIL_MS=1 run verdict >"$tmp/verdict-budget-tail.out" 2>&1 || code=$?
expect 1 "$code" "a read slow every time still fails, whichever read it is"
expect_grep '^FAIL time-budget.*tail' "$tmp/verdict-budget-tail.out" "the slow read is named"

# A planned restart's drop delta fails the same way a new reason does.
verdict_fixture "" "[{\"ts\":\"$(iso_ago $((5 * DAY)))\",\"serving_ms\":4000,\"new_drops\":1}]"
printf '%s conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)\n' \
    "$(iso_ago $((5 * DAY - 4)))" >>"$home/.conbus/daemon.log"
code=0
run verdict >"$tmp/verdict-rdrop.out" 2>&1 || code=$?
expect 1 "$code" "a drop during a planned restart fails"
expect_grep '^FAIL drops' "$tmp/verdict-rdrop.out" "the planned-restart drop is named FAIL"

# A drop log at the 4 MiB cap went blind — every later drop was silently
# discarded, so drops can never be PASS on it. The live log did exactly
# this for seven days in October; preflight must refuse a swap over it too.
verdict_fixture
# perl, not `yes | head` — head exits mid-stream and yes dies on SIGPIPE,
# which set -o pipefail turns into a rehearsal failure that is not the
# scenario's.
perl -e 'print "{\"ts\":\"2026-10-10T09:00:00.000Z\",\"reason\":\"old flood\"}\n" x 80000' \
    >"$home/.conbus/drops.jsonl"
code=0
run verdict >"$tmp/verdict-capped.out" 2>&1 || code=$?
expect 4 "$code" "a capped drop log leaves the verdict unchecked, not passed"
expect_grep '^UNCHECKED drops' "$tmp/verdict-capped.out" \
    "a drops.jsonl at the cap cannot pass drops"
expect_grep 'verdict: UNCHECKED' "$tmp/verdict-capped.out" "the outcome is named"
expect_grep 'canary:   drops — ' "$tmp/verdict-capped.out" "with the reason, not only the item"
expect_no_grep 'verdict: PASS' "$tmp/verdict-capped.out" "an unchecked drops line cannot read as a pass"
if grep -q '^PASS drops' "$tmp/verdict-capped.out"; then
    echo "rehearsal: FAIL: a capped drops.jsonl passed drops" >&2
    exit 1
fi
code=0
run preflight >"$tmp/preflight-capped.out" 2>&1 || code=$?
expect 1 "$code" "preflight refuses a swap over a blind drop log"
expect_grep '^FAIL drops-cap' "$tmp/preflight-capped.out" \
    "the capped drops.jsonl is the named failure"

# A rotated log still sees both sides: the new reason lives only in the
# generation that was renamed away — a flood mid-window rotates the file
# and the fresh drops.jsonl holds nothing new. Reading drops.jsonl alone
# misses it.
verdict_fixture
cat "$home/.conbus/drops.jsonl" >"$home/.conbus/drops.1.jsonl"
printf '{"ts":"%s","reason":"a fault after the rotation"}\n' "$(iso_ago 3600)" >>"$home/.conbus/drops.1.jsonl"
code=0
run verdict >"$tmp/verdict-rotated.out" 2>&1 || code=$?
expect 1 "$code" "a new drop reason in the rotated generation is caught"
expect_grep 'a fault after the rotation' "$tmp/verdict-rotated.out" \
    "the post-rotation reason is named"
rm -f "$home/.conbus/drops.1.jsonl"

# --- what a drop line means ---------------------------------------------------

drop_line() { # drop_line <seconds ago> <reason, JSON-escaped> [detail]
    if [ -n "${3:-}" ]; then
        printf '{"detail":"%s","reason":"%s","ts":"%s"}\n' "$3" "$2" "$(iso_ago "$1")"
    else
        printf '{"reason":"%s","ts":"%s"}\n' "$2" "$(iso_ago "$1")"
    fi >>"$home/.conbus/drops.jsonl"
}

# A lost record fails whether or not its words have been seen before. The swap
# window aside, a week of these used to read `0 new drop reason(s)`.
for lost_reason in \
    'no daemon on /x/sock: Connection refused (os error 61)' \
    'no reply from the daemon: Resource temporarily unavailable (os error 35)' \
    'could not send the record: Broken pipe (os error 32)' \
    'could not bound the daemon call: Invalid argument (os error 22)' \
    'unreadable reply \"\": EOF while parsing a value at line 1 column 0' \
    'the daemon refused the record: 2 record(s) written, 1 lost: No space left on device (os error 28)' \
    'spooled hook refused: 1 record(s) written, 1 lost: No space left on device (os error 28)'; do
    verdict_fixture
    drop_line $((8 * DAY)) "$lost_reason"
    drop_line 3600 "$lost_reason"
    code=0
    run verdict >"$tmp/verdict-lost.out" 2>&1 || code=$?
    expect 1 "$code" "a known reason that lost a record fails: ${lost_reason%%:*}"
    expect_grep '^FAIL drops — 1 lost record' "$tmp/verdict-lost.out" "and is counted as lost"
    expect_grep 'fail at any count' "$tmp/verdict-lost.out" "and the rule is printed beside it"
done

# The swap window keeps its exemption for its two reasons, known or not, and
# only for 60 seconds.
verdict_fixture "$(iso_ago 120)"
drop_line $((3 * DAY)) 'no daemon on /x/sock: Connection refused (os error 61)'
drop_line 100 'no daemon on /x/sock: Connection refused (os error 61)'
code=0
run verdict >"$tmp/verdict-swap-nd.out" 2>&1 || code=$?
expect 3 "$code" "a no-daemon line inside 60 s of the swap is exempt"
expect_grep '^PASS drops.*1 expected swap-window' "$tmp/verdict-swap-nd.out" "and counted"
drop_line 30 'no daemon on /x/sock: Connection refused (os error 61)'
code=0
run verdict >"$tmp/verdict-swap-nd90.out" 2>&1 || code=$?
expect 1 "$code" "the same known reason 90 s after the swap fails"

# Every event name a vendor renames into is its own gap. The reason's escaped
# quote used to end the parse, so all of them read as one known prefix.
verdict_fixture
drop_line $((8 * DAY)) 'no mapping for hook event \"KnownOld\" (as claude_code, fields: cwd,hook_event_name)'
drop_line 3600 'no mapping for hook event \"StopV2\" (as claude_code, fields: cwd,hook_event_name)'
code=0
run verdict >"$tmp/verdict-renamed.out" 2>&1 || code=$?
expect 1 "$code" "a renamed event fails"
expect_grep 'StopV2 as claude_code' "$tmp/verdict-renamed.out" "the new event and harness are named"
verdict_fixture
drop_line $((8 * DAY)) 'no mapping for hook event \"KnownOld\" (as claude_code, fields: cwd,hook_event_name)'
drop_line 3600 'no mapping for hook event \"KnownOld\" (as claude_code, fields: cwd,hook_event_name)'
code=0
run verdict >"$tmp/verdict-renamed-known.out" 2>&1 || code=$?
expect 1 "$code" "an event already unmapped before the swap still lost its record"

# Sessions the log never had: their end is not a lost record, whatever its
# spelling. These are most of the real orphan lines, and one would have rolled a
# swap back or failed a week. A tool event in one is real content, and fails.
verdict_fixture
for ghost in 'SessionEnd for session 01a06300' 'sessionEnd for session 01a06301' 'Stop for session 01a06302'; do
    drop_line 3600 'no conversation for this session; the daemon has no binding for it (a restart mid-turn looks exactly like this)' "$ghost"
done
drop_line 3600 'spooled hook still has no conversation (orphan)' 'session_ended for session 01a06303'
code=0
run verdict >"$tmp/verdict-ghost.out" 2>&1 || code=$?
expect 0 "$code" "the end of a session the log never had is not a loss"
expect_grep '^PASS drops.*4 not lost' "$tmp/verdict-ghost.out" "and it is shown, not dropped from view"
expect_grep '1× session_ended' "$tmp/verdict-ghost.out" "by event"
expect_grep '^  rule: a lost record or adapter gap fails at any count' "$tmp/verdict-ghost.out" "and the standard is printed for a person to argue with"
expect_no_grep '01a0630' "$tmp/verdict-ghost.out" "without a session id"
verdict_fixture
drop_line 3600 'spooled hook still has no conversation (orphan)' 'tool_use for session 01a06304'
code=0
run verdict >"$tmp/verdict-orphan.out" 2>&1 || code=$?
expect 1 "$code" "a tool event with no conversation is a lost record"
expect_grep '^FAIL drops — 1 lost record' "$tmp/verdict-orphan.out" "and is counted as one"
expect_grep '1× tool_use: spooled hook' "$tmp/verdict-orphan.out" "by event"
expect_no_grep '01a0630' "$tmp/verdict-orphan.out" "without a session id"

# A payload that names no session is the vendor's, per the plan: shown, and it
# stops a pass only as a flood. Same for a payload the hook could not parse.
verdict_fixture
drop_line 3600 'the hook payload names no session; an empty id would merge unrelated sessions into one conversation' 'preToolUse as cursor (fields: cwd,tool_name)'
drop_line 3600 'EOF while parsing a value at line 1 column 0'
code=0
run verdict >"$tmp/verdict-sessionless.out" 2>&1 || code=$?
expect 0 "$code" "a sessionless line or an unparseable payload alone does not fail"
expect_grep '1× preToolUse as cursor' "$tmp/verdict-sessionless.out" "the sessionless line is shown"
expect_no_grep 'fields: cwd' "$tmp/verdict-sessionless.out" "without its field list"
for _ in 1 2 3; do
    drop_line 3600 'the hook payload names no session; an empty id would merge unrelated sessions into one conversation' 'preToolUse as cursor (fields: cwd,tool_name)'
done
code=0
CONBUS_CANARY_NOISE_MAX=3 run verdict >"$tmp/verdict-flood.out" 2>&1 || code=$?
expect 4 "$code" "a flood of sessionless lines is not a pass"
expect_grep '^UNCHECKED drops.*over the limit of 3' "$tmp/verdict-flood.out" "the limit it crossed is named"
expect_grep 'canary:   drops — ' "$tmp/verdict-flood.out" "and listed as what is open"
code=0
CONBUS_CANARY_NOISE_MAX=lots run verdict >/dev/null 2>&1 || code=$?
expect 2 "$code" "a limit that is not a number is refused"

# A full disk that the daemon outlasted (#155). Every record waited and landed, so
# that line is no loss. The same outage with records it could not keep is one.
verdict_fixture
drop_line 3600 'the log refused writes for a while' '3 record(s) waited in memory and landed, 0 could not be kept there, since 2026-11-01T00:00:00.000Z: No space left on device (os error 28)'
code=0
run verdict >"$tmp/verdict-held.out" 2>&1 || code=$?
expect 0 "$code" "an outage in which every record landed is not a loss"
expect_grep '^PASS drops.*1 not lost' "$tmp/verdict-held.out" "it is counted as not lost"
expect_grep 'every record waited and landed' "$tmp/verdict-held.out" "and shown"
verdict_fixture
drop_line 3600 'the log refused writes for a while and records may be lost' '3 record(s) waited in memory and landed, 2 could not be kept there, since 2026-11-01T00:00:00.000Z: No space left on device (os error 28)'
code=0
run verdict >"$tmp/verdict-held-lost.out" 2>&1 || code=$?
expect 1 "$code" "an outage that could not keep its records fails"
expect_grep '^FAIL drops — 1 lost record' "$tmp/verdict-held-lost.out" "as a lost record"

# The same words, seen before and covered by no rule, need a person.
verdict_fixture
drop_line $((8 * DAY)) 'a reason no rule covers'
drop_line 3600 'a reason no rule covers'
code=0
run verdict >"$tmp/verdict-known.out" 2>&1 || code=$?
expect 4 "$code" "a known reason no rule covers is not a pass"
expect_grep '^UNCHECKED drops.*no rule covers' "$tmp/verdict-known.out" "and says why"

# A line that cannot be read is not a line without a problem, and one bad byte
# must not cost the read of the line it sits on.
verdict_fixture
printf '{"reason":"no daem\n' >>"$home/.conbus/drops.jsonl"
code=0
run verdict >"$tmp/verdict-torn.out" 2>&1 || code=$?
expect 4 "$code" "a torn drop line is not a pass"
expect_grep '^UNCHECKED drops.*no readable time or reason' "$tmp/verdict-torn.out" "the torn line is counted"
verdict_fixture
printf '{"reason":"no daemon on /x/\377/sock: Connection refused (os error 61)","ts":"%s"}\n' "$(iso_ago 3600)" \
    >>"$home/.conbus/drops.jsonl"
code=0
LC_ALL=en_US.UTF-8 run verdict >"$tmp/verdict-badbyte.out" 2>&1 || code=$?
expect 1 "$code" "an invalid byte does not hide the lost record on its line"
expect_grep '^FAIL drops — 1 lost record' "$tmp/verdict-badbyte.out" "the line is counted, under a UTF-8 locale too"
expect_grep 'verdict: FAIL' "$tmp/verdict-badbyte.out" "and the verdict reaches its summary"

# A capped log still shows what it recorded before it filled. A new reason on
# day 2 plus a cap on day 5 used to read as a pass, and main said FAIL.
fill_to_cap() { # fill_to_cap <ts> — plain perl: `yes | head` dies on SIGPIPE under pipefail
    perl -e '
        my ($file, $ts) = @ARGV;
        my $line = "{\"reason\":\"the hook payload names no session; an empty id would merge unrelated sessions into one conversation\",\"ts\":\"$ts\"}\n";
        my $n = int((4 * 1024 * 1024 - (-s $file)) / length($line)) + 1;
        open(my $fh, ">>", $file) or die; print $fh $line x $n; close $fh;' "$home/.conbus/drops.jsonl" "$1"
}
verdict_fixture
drop_line 3600 'a brand new fault after the swap'
fill_to_cap "$(iso_ago 1800)"
code=0
run verdict >"$tmp/verdict-capped-new.out" 2>&1 || code=$?
expect 1 "$code" "a capped log does not hide a new reason written before the cap"
expect_grep 'a brand new fault after the swap' "$tmp/verdict-capped-new.out" "the reason is named"
verdict_fixture "" "[{\"ts\":\"$(iso_ago $((5 * DAY)))\",\"serving_ms\":4000,\"new_drops\":7}]"
printf '%s conbus daemon: beef: socket /x/sock log /x/log (10 record(s), 0 unreadable)\n' \
    "$(iso_ago $((5 * DAY - 4)))" >>"$home/.conbus/daemon.log"
fill_to_cap "$(iso_ago 1800)"
code=0
run verdict >"$tmp/verdict-capped-restart.out" 2>&1 || code=$?
expect 1 "$code" "a capped log does not hide drops on a planned restart"
expect_grep '^FAIL drops.*7 drop(s) during planned restarts' "$tmp/verdict-capped-restart.out" "they are named"
verdict_fixture
fill_to_cap "$(iso_ago $((30 * DAY)))"
code=0
run verdict >"$tmp/verdict-capped-blind.out" 2>&1 || code=$?
expect 4 "$code" "a capped log with nothing to show is still not a pass"
expect_grep '^UNCHECKED drops.*does not rotate: nothing after' "$tmp/verdict-capped-blind.out" \
    "it says when the log went blind"

# The same file under a build that rotates is not blind: the next drop renames it.
write_fake beef rotating "$tmp/stage/conbus-rotating"
verdict_fixture
write_fake beef rotating "$install_dir/conbus"
fill_to_cap "$(iso_ago $((30 * DAY)))"
code=0
run verdict >"$tmp/verdict-capped-rotating.out" 2>&1 || code=$?
expect 0 "$code" "a full log on a build that rotates is not blind"
expect_grep '^PASS drops' "$tmp/verdict-capped-rotating.out" "and drops can pass on it"
write_fake beef live "$install_dir/conbus"
code=0
run preflight --from "$tmp/stage/conbus-rotating" >"$tmp/preflight-rotating.out" 2>&1 || code=$?
expect_grep '^PASS drops-cap' "$tmp/preflight-rotating.out" "preflight lets a rotating candidate swap over it"

# Just rotated, no live file yet: the new reason is only in drops.1.
verdict_fixture
mv "$home/.conbus/drops.jsonl" "$home/.conbus/drops.1.jsonl"
printf '{"reason":"a fault in the old generation","ts":"%s"}\n' "$(iso_ago 3600)" >>"$home/.conbus/drops.1.jsonl"
code=0
run verdict >"$tmp/verdict-rotated-only.out" 2>&1 || code=$?
expect 1 "$code" "a new reason only in drops.1 fails with no live file"
expect_grep 'a fault in the old generation' "$tmp/verdict-rotated-only.out" "the reason is named"

# One rotation keeps the baseline, and a clean window still passes across it.
verdict_fixture
mv "$home/.conbus/drops.jsonl" "$home/.conbus/drops.1.jsonl"
drop_line 3600 'spooled hook still has no conversation (orphan)' 'session_ended for session 01a06305'
code=0
run verdict >"$tmp/verdict-one-rotation.out" 2>&1 || code=$?
expect 0 "$code" "one rotation loses nothing"

# Two rotations threw the baseline and part of the window away. Known kinds are
# still known, which ends the false FAIL, and the hole in the evidence is named.
verdict_fixture
printf '{"reason":"the hook payload names no session; an empty id would merge unrelated sessions into one conversation","ts":"%s"}\n' \
    "$(iso_ago $((3 * DAY)))" >"$home/.conbus/drops.1.jsonl"
rm -f "$home/.conbus/drops.jsonl"
drop_line 3600 'the hook payload names no session; an empty id would merge unrelated sessions into one conversation'
code=0
run verdict >"$tmp/verdict-two-rotations.out" 2>&1 || code=$?
expect 4 "$code" "two rotations leave a hole, not a pass and not a fail"
expect_grep '^UNCHECKED drops.*anything dropped before it was rotated away' "$tmp/verdict-two-rotations.out" "the hole is named"
expect_no_grep '^FAIL drops' "$tmp/verdict-two-rotations.out" "a known kind does not read as new"
printf '{"reason":"a reason no rule covers","ts":"%s"}\n' "$(iso_ago $((3 * DAY)))" >"$home/.conbus/drops.1.jsonl"
code=0
run verdict >"$tmp/verdict-two-rotations-other.out" 2>&1 || code=$?
expect 4 "$code" "with the baseline gone, an unknown reason is a question"
expect_grep 'the baseline that would say whether it is new was rotated away' "$tmp/verdict-two-rotations-other.out" "and the missing baseline is said"

# tool bodies elided (#158): the record landed, so it is no drop and no new
# reason. Its count is judged alone, against the tool calls in the window.
elided_line() { # elided_line <N>
    drop_line 3600 'tool bodies elided, not sealed' "$1 tool event(s)"
}
verdict_fixture
elided_line 3
elided_line 2
code=0
run verdict >"$tmp/verdict-elided-low.out" 2>&1 || code=$?
expect 0 "$code" "elided bodies below 1% of tool calls pass"
expect_grep '^PASS tool-bodies — 5 tool event(s)' "$tmp/verdict-elided-low.out" "their sum is on its own line"
expect_grep '^PASS drops — 0 lost' "$tmp/verdict-elided-low.out" "and the line is not a drop"
expect_no_grep 'new drop\|not seen before' "$tmp/verdict-elided-low.out" "and not a new reason"
verdict_fixture
elided_line 100000
code=0
run verdict >"$tmp/verdict-elided-high.out" 2>&1 || code=$?
expect 1 "$code" "elided bodies above 1% of tool calls fail"
expect_grep '^FAIL tool-bodies — 100000 tool event(s)' "$tmp/verdict-elided-high.out" "the item that failed says so"
expect_grep '^PASS drops' "$tmp/verdict-elided-high.out" "and it is tool-bodies that fails, not drops"
# 1% exactly is within it, one more is not. One day holds all the tool calls.
verdict_fixture
sed -E 's/"tool_calls":[0-9]+/"tool_calls":0/' "$home/.usage-rows" >"$home/.usage-rows.new"
printf '{"day":"%s","harness":"codex","turns":3,"tool_calls":100,"tool_denials":0,"repeated":0,"recovered":0,"tokens_input":1}\n' \
    "$(iso_ago $((2 * DAY)) | cut -c1-10)" >>"$home/.usage-rows.new"
mv -f "$home/.usage-rows.new" "$home/.usage-rows"
elided_line 1
code=0
run verdict >"$tmp/verdict-elided-1pct.out" 2>&1 || code=$?
expect_grep '^PASS tool-bodies — 1 tool event' "$tmp/verdict-elided-1pct.out" "exactly 1% is within the limit"
elided_line 1
code=0
run verdict >"$tmp/verdict-elided-over.out" 2>&1 || code=$?
expect_grep '^FAIL tool-bodies — 2 tool event' "$tmp/verdict-elided-over.out" "more than 1% is not"
# Both generations count, and a count that cannot be read cannot pass.
verdict_fixture
cat "$home/.conbus/drops.jsonl" >"$home/.conbus/drops.1.jsonl"
printf '{"detail":"4 tool event(s)","reason":"tool bodies elided, not sealed","ts":"%s"}\n' "$(iso_ago 3600)" >>"$home/.conbus/drops.1.jsonl"
elided_line 2
code=0
run verdict >"$tmp/verdict-elided-both.out" 2>&1 || code=$?
expect_grep '^PASS tool-bodies — 6 tool event' "$tmp/verdict-elided-both.out" "the sum spans both generations"
verdict_fixture
elided_line 3
touch "$home/.usage-fail"
code=0
run verdict >"$tmp/verdict-elided-nousage.out" 2>&1 || code=$?
rm -f "$home/.usage-fail"
expect 4 "$code" "no tool-call count is not a pass"
expect_grep '^UNCHECKED tool-bodies — 3 tool event' "$tmp/verdict-elided-nousage.out" "tool-bodies says why"
# The count that matters is the window's. A busy month before the swap must not
# dilute a week in which every tool body was elided.
verdict_fixture
awk -v s="$(iso_after "$fx_swapped" 0 | cut -c1-10)" 'match($0, /"day":"[^"]*"/) && substr($0, RSTART + 7, 10) >= s' \
    "$home/.usage-rows" | sed -E 's/"tool_calls":[0-9]+/"tool_calls":0/' >"$home/.usage-window"
printf '{"day":"%s","harness":"codex","turns":3,"tool_calls":100,"tool_denials":0,"repeated":0,"recovered":0}\n' \
    "$(iso_ago $((2 * DAY)) | cut -c1-10)" >>"$home/.usage-window"
elided_line 2
code=0
run verdict >"$tmp/verdict-elided-window.out" 2>&1 || code=$?
expect_grep '^FAIL tool-bodies — 2 tool event.*of 100 tool call' "$tmp/verdict-elided-window.out" "the limit is 1% of the tool calls since the swap"
rm -f "$home/.usage-window"
# A row that names no tool_calls is a count not read, not a count of zero.
verdict_fixture
sed -E 's/"tool_calls":[0-9]+,//' "$home/.usage-rows" >"$home/.usage-rows.new"
mv -f "$home/.usage-rows.new" "$home/.usage-rows"
code=0
run verdict >"$tmp/verdict-elided-nocount.out" 2>&1 || code=$?
expect 4 "$code" "usage rows with no tool_calls are not a pass"
expect_grep '^UNCHECKED tool-bodies.*name no tool_calls' "$tmp/verdict-elided-nocount.out" "and tool-bodies says why"
verdict_fixture
drop_line 3600 'tool bodies elided, not sealed' 'many'
code=0
run verdict >"$tmp/verdict-elided-garbled.out" 2>&1 || code=$?
expect 4 "$code" "a detail that does not carry a count is not a pass"
expect_grep '^UNCHECKED tool-bodies' "$tmp/verdict-elided-garbled.out" "and says the count is not known"

# C0 — verdict items that could pass over a blind input say UNCHECKED.
# A daemon.log whose every start line predates the swap cannot prove
# anything about restarts inside the window.
verdict_fixture
printf '%s conbus daemon: 01d: socket /x/sock log /x/log (10 record(s), 0 unreadable)\n' \
    "$(iso_ago $((30 * DAY)))" >"$home/.conbus/daemon.log"
code=0
run verdict >"$tmp/verdict-nostarts.out" 2>&1 || code=$?
expect 4 "$code" "an unchecked restarts line is not a pass"
expect_grep '^UNCHECKED restarts' "$tmp/verdict-nostarts.out" \
    "a log that does not reach the swap cannot pass restarts"
if grep -q '^PASS restarts' "$tmp/verdict-nostarts.out"; then
    echo "rehearsal: FAIL: a pre-swap-only daemon.log passed restarts" >&2
    exit 1
fi

# A parity of emptiness: both binaries answering nothing "agree" on every
# closed day — that proved nothing. Rows with no day inside the window
# leave twins nothing to compare either.
verdict_fixture
: >"$home/.usage-rows"
code=0
run verdict >"$tmp/verdict-blind.out" 2>&1 || code=$?
expect 4 "$code" "unchecked parity and twins are not a pass"
expect_grep '^UNCHECKED usage-parity' "$tmp/verdict-blind.out" \
    "two empties do not pass parity"
expect_grep '^UNCHECKED twins-repeated' "$tmp/verdict-blind.out" \
    "no in-window rows do not pass twins-repeated"

# Rows that all predate the swap are not "every harness on all 0 days".
verdict_fixture
awk -v s="$(iso_ago $((8 * DAY)))" 'match($0, /"day":"[^"]*"/) && substr($0, RSTART + 7, 10) < substr(s, 1, 10)' \
    "$home/.usage-rows" >"$home/.usage-rows.old"
mv -f "$home/.usage-rows.old" "$home/.usage-rows"
code=0
run verdict >"$tmp/verdict-norows.out" 2>&1 || code=$?
expect 4 "$code" "no record since the swap is not a pass"
expect_grep '^UNCHECKED harness-activity' "$tmp/verdict-norows.out" \
    "zero days in the window cannot pass harness-activity"

# One side of the parity failing on one day compared nothing that day; the
# text of two failures must not count as agreement.
verdict_fixture
iso_ago $((3 * DAY)) | cut -c1-10 >"$home/.usage-fail-day"
code=0
run verdict >"$tmp/verdict-parityfail.out" 2>&1 || code=$?
expect 4 "$code" "a parity read that failed on one day is not a pass"
expect_grep "^UNCHECKED usage-parity.*$(cat "$home/.usage-fail-day")" "$tmp/verdict-parityfail.out" \
    "the day that could not be read is named"
rm -f "$home/.usage-fail-day"

# A log that cannot be read: usage fails and every item built on it says so.
# Timing a read that fails fast proves no budget either. That was a PASS.
verdict_fixture
touch "$home/.usage-fail"
code=0
run verdict >"$tmp/verdict-usagefail.out" 2>&1 || code=$?
expect 4 "$code" "a usage read that fails is not a pass"
expect_grep '^UNCHECKED harness-activity.*a segment could not be read' "$tmp/verdict-usagefail.out" \
    "the failing read is named, with what it said"
expect_grep '^UNCHECKED twins-repeated' "$tmp/verdict-usagefail.out" "twins needs that read"
expect_grep '^UNCHECKED usage-parity' "$tmp/verdict-usagefail.out" "parity needs that read"
expect_grep '^UNCHECKED time-budget' "$tmp/verdict-usagefail.out" "a failing read cannot be timed"
rm -f "$home/.usage-fail"

# Each file the verdict reads, unreadable: the verdict still finishes with a
# summary, never dies mid-way, and never passes. Skipped as root, who reads all.
if [ "$(id -u)" != 0 ]; then
    for unreadable_input in daemon.log daemon.build drops.jsonl canary.json; do
        verdict_fixture
        chmod 000 "$home/.conbus/$unreadable_input"
        code=0
        run verdict >"$tmp/verdict-chmod.out" 2>&1 || code=$?
        chmod 600 "$home/.conbus/$unreadable_input"
        case $code in
            1 | 4) echo "rehearsal: ok: an unreadable $unreadable_input is not a pass (exit $code)" ;;
            *)
                echo "rehearsal: FAIL: an unreadable $unreadable_input gave exit $code" >&2
                cat "$tmp/verdict-chmod.out" >&2
                exit 1
                ;;
        esac
        expect_grep 'verdict: ' "$tmp/verdict-chmod.out" \
            "an unreadable $unreadable_input still reaches a summary"
    done
fi

if [ "$(id -u)" != 0 ]; then
    verdict_fixture
    chmod 000 "$home/.conbus/canary.json"
    code=0
    run verdict >"$tmp/verdict-chmod-state.out" 2>&1 || code=$?
    chmod 600 "$home/.conbus/canary.json"
    expect 4 "$code" "an unreadable canary.json ends in exit 4"
    expect_grep 'canary.json cannot be read' "$tmp/verdict-chmod-state.out" "and says so, not that no swap was recorded"
fi

# A dead recorder pid is this item's whole point, and `ps` failing inside a
# pipeline under `set -e` used to end the script before the line printed.
verdict_fixture
sleep 0 &
dead_pid=$!
wait "$dead_pid"
printf '{"build":"beef","pid":%s,"started":"%s","serving":"%s"}\n' "$dead_pid" \
    "$(iso_ago $((7 * DAY)))" "$(iso_ago $((7 * DAY - 2)))" >"$home/.conbus/daemon.build"
code=0
run verdict >"$tmp/verdict-deadpid.out" 2>&1 || code=$?
expect 1 "$code" "a dead recorder pid fails the verdict"
expect_grep '^FAIL rss-budget' "$tmp/verdict-deadpid.out" "the dead recorder is named"
expect_grep 'verdict: FAIL' "$tmp/verdict-deadpid.out" "and the summary still prints"

# C1 — verdict paths the bash -x trace showed the rehearsal never reached
# (evidence/c1-unrun-lines.md). A day-only --since bounds the window at
# UTC midnight of that day; it is a look at the window, never its verdict.
past=$(iso_ago $((7 * DAY + 3600)))
verdict_fixture "$past"
code=0
run verdict --since "${past:0:10}" >"$tmp/verdict-since.out" 2>&1 || code=$?
expect 4 "$code" "--since other than the swap is a look, not a verdict"
expect_grep "since ${past:0:10}T00:00:00.000Z" "$tmp/verdict-since.out" \
    "the day-only --since becomes a UTC midnight boundary"
expect_grep '^UNCHECKED window' "$tmp/verdict-since.out" "the override is named"
code=0
run verdict --since "$past" >"$tmp/verdict-since-same.out" 2>&1 || code=$?
expect 0 "$code" "--since naming the recorded swap is the verdict itself"

# --since cannot complete a window: the clock runs from the recorded swap.
verdict_fixture "$(iso_ago $((2 * DAY)))"
code=0
run verdict --since "$(iso_ago $((3 * DAY)))" >"$tmp/verdict-since-old.out" 2>&1 || code=$?
expect 3 "$code" "an old --since does not complete a window that is still open"
expect_grep 'window incomplete (2/7' "$tmp/verdict-since-old.out" "counted from the swap, not from --since"
# And a boundary that is not a time is refused rather than compared: as text,
# `yesterday` sorts after every timestamp, so nothing was ever "after" it.
code=0
run verdict --since yesterday >"$tmp/verdict-since-word.out" 2>&1 || code=$?
expect 2 "$code" "--since yesterday is refused"
code=0
run verdict --since 2026-13-45 >/dev/null 2>&1 || code=$?
expect 2 "$code" "--since that is shaped like a date and is not one is refused"
# `date` would read this one with a warning, and it sorts wrongly against the logs.
code=0
run verdict --since 2026-10-01T00:00:00+09:00 >/dev/null 2>&1 || code=$?
expect 2 "$code" "--since with an offset is refused: every stamp here is UTC text"

# No recorded swap: nothing says a window began, whatever the stamp says.
verdict_fixture
rm -f "$home/.conbus/canary.json"
code=0
run verdict --since "$past" >"$tmp/verdict-nostate.out" 2>&1 || code=$?
expect 4 "$code" "no canary.json cannot end in a pass"
expect_grep '^UNCHECKED window.*no recorded swap' "$tmp/verdict-nostate.out" "the missing swap is named"
# A swap time that is not a time ends the verdict, it does not skip the check.
printf '{"swapped":"soon","prev_build":"01d","candidate_build":"beef","candidate_sha256":"x","restarts":[]}\n' \
    >"$home/.conbus/canary.json"
code=0
run verdict >"$tmp/verdict-badswap.out" 2>&1 || code=$?
expect 4 "$code" "a swap time that is not a time cannot end in a pass"
expect_grep '^UNCHECKED window' "$tmp/verdict-badswap.out" "the bad swap time is named"

# Parity has no 0.2.8 side at all when the rollback copy is gone — that is
# a FAIL, not an agreement of empties.
verdict_fixture
rm -f "$rollback_bin"
code=0
run verdict >"$tmp/verdict-noroll.out" 2>&1 || code=$?
expect 1 "$code" "a missing rollback binary fails the parity check"
expect_grep '^FAIL usage-parity' "$tmp/verdict-noroll.out" \
    "the missing 0.2.8 side is named"

# A daemon.log without the "(N record(s), N unreadable)" parenthetical
# falls back to the doctor's own log line for the unreadable count.
verdict_fixture
printf '%s conbus daemon: beef: socket /x/sock log /x/log\n' \
    "$(iso_after "$fx_swapped" 4)" >"$home/.conbus/daemon.log"
code=0
run verdict >"$tmp/verdict-unread.out" 2>&1 || code=$?
expect 0 "$code" "the doctor-fallback unreadable count completes the window"
expect_grep '^PASS unreadable' "$tmp/verdict-unread.out" \
    "the unreadable count falls back to the doctor's log line"

# A joined machine's sync POSTs are operator-visible, not a silent pass.
verdict_fixture
touch "$home/.conbus/sync.json"
code=0
run verdict >"$tmp/verdict-sync.out" 2>&1 || code=$?
expect 4 "$code" "an unchecked sync line is not a pass"
expect_grep '^UNCHECKED sync' "$tmp/verdict-sync.out" \
    "a joined machine's POSTs need counting"
rm -f "$home/.conbus/sync.json"

# A policy file that cannot be a policy decides nothing: zero counted
# denials over it is not a pass.
verdict_fixture
write_fake beef polfail "$install_dir/conbus"
echo 'this is not a policy' >"$home/.conbus/policy.json"
code=0
run verdict >"$tmp/verdict-polfail.out" 2>&1 || code=$?
expect 1 "$code" "a policy file that cannot decide fails the verdict"
expect_grep '^FAIL policy' "$tmp/verdict-polfail.out" \
    "the unparseable policy is the named failure"
rm -f "$home/.conbus/policy.json"

# time-budget: one slow doctor is a spike, not a wedge. It gets two more reads
# and the median judges, so the verdict passes and doctor ran 3x.
verdict_fixture
write_fake beef flap "$install_dir/conbus"
rm -f "$home/.flap"
code=0
DOCTOR_MS=2000 run verdict >"$tmp/verdict-median.out" 2>&1 || code=$?
expect 0 "$code" "one slow doctor out of three does not fail the verdict"
expect_grep '^PASS time-budget' "$tmp/verdict-median.out" \
    "the median of three passes the time budget"
expect 3 "$(cat "$home/.flap")" "doctor was measured three times"
# A doctor that makes its budget is not read again: each read starts vendors.
code=0
DOCTOR_MS=2000 run verdict >"$tmp/verdict-oneread.out" 2>&1 || code=$?
expect 0 "$code" "a fast doctor passes on its first read"
expect 4 "$(cat "$home/.flap")" "and is read only once"

# Inside the window the verdict is "incomplete", not a pass.
verdict_fixture "$(date -u +%Y-%m-%dT00:00:00.000Z)"
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

# F5 — new_drops counts drop LINES added during the restart, not new reason
# strings. A planned restart that drops one record of an already-known
# reason must still be recorded (and fail the verdict).
past=$(date -u -v-7d -v-1H +%Y-%m-%dT%H:%M:%S.000Z 2>/dev/null || date -u -d '7 days ago 1 hour ago' +%Y-%m-%dT%H:%M:%S.000Z)
verdict_fixture
cat >"$home/.conbus/canary.json" <<EOF
{"swapped":"$past","prev_build":"01d","candidate_build":"beef","candidate_sha256":"x","restarts":[]}
EOF
write_fake beef knowndrop "$install_dir/conbus"
daemon_boot beef 5010
code=0
run_real restart >"$tmp/restart-knowndrop.out" 2>&1 || code=$?
expect 0 "${code:-0}" "planned restart over the real path"
expect_grep '1 drop' "$tmp/restart-knowndrop.out" "a known-reason drop is counted as a loss"
expect_grep '"new_drops":1' "$home/.conbus/canary.json" "canary.json records the lost count"
code=0
run verdict >"$tmp/verdict-knowndrop.out" 2>&1 || code=$?
expect 1 "$code" "a lost record on a planned restart fails the verdict"
expect_grep '^FAIL drops' "$tmp/verdict-knowndrop.out" "the restart loss is named"

# A sessionless refusal during a planned restart is shown aside, not
# counted — the vendor sent a payload with no session, nothing was lost.
cat >"$home/.conbus/canary.json" <<EOF
{"swapped":"$past","prev_build":"01d","candidate_build":"beef","candidate_sha256":"x","restarts":[]}
EOF
write_fake beef sessions "$install_dir/conbus"
daemon_boot beef 5011
code=0
run_real restart >"$tmp/restart-sessions.out" 2>&1 || code=$?
expect 0 "${code:-0}" "planned restart with a sessionless refusal"
expect_grep 'sessionless' "$tmp/restart-sessions.out" "the sessionless refusal is shown aside"
expect_grep '"new_drops":0' "$home/.conbus/canary.json" "it is not counted as a loss"

# Tool bodies elided on a planned restart: the records landed, so new_drops
# stays 0 however many tool calls were in flight.
cat >"$home/.conbus/canary.json" <<EOF
{"swapped":"$past","prev_build":"01d","candidate_build":"beef","candidate_sha256":"x","restarts":[]}
EOF
write_fake beef elided "$install_dir/conbus"
daemon_boot beef 5012
code=0
run_real restart >"$tmp/restart-elided.out" 2>&1 || code=$?
expect 0 "${code:-0}" "planned restart with elided tool bodies"
expect_grep '"new_drops":0' "$home/.conbus/canary.json" "elided bodies are not counted in new_drops"
expect_grep '2 line(s) that lost no record not counted' "$tmp/restart-elided.out" "they are shown aside"

# rollback reports the spool it leaves behind.
mkdir -p "$home/.conbus/spool"
touch "$home/.conbus/spool/req-1" "$home/.conbus/spool/req-2"
write_fake 01d old "$rollback_bin"
run rollback >"$tmp/rollback.out" 2>&1
expect 0 $? "rollback"
expect "conbus 0.0.0-01d (01d)" "$("$install_dir/conbus" --version 2>&1 || echo x)" "rollback restored the original"
expect_absent "$rollback_bin" "conbus.rollback is consumed by the mv back"
expect_grep 'spool: 2 file' "$tmp/rollback.out" "rollback reports the spool left behind"
code=0
run rollback >/dev/null 2>&1 || code=$?
expect 1 "$code" "rollback refuses when conbus.rollback is missing"
code=0
run start --from "$tmp/stage/nonexistent" >/dev/null 2>&1 || code=$?
expect 1 "$code" "start refuses a missing candidate"

# A real build under this throwaway HOME would ask an agy that is not signed
# in, and a build older than the sign-in check opens a browser tab doing it.
expect_file "$tmp/agy-bin.seen" "the fakes saw doctor run"
expect 0 "$(grep -cv '^/usr/bin/false$' "$tmp/agy-bin.seen")" "no doctor in a rehearsal can reach a real agy"

# The real path asks agy instead, because the installed build may be that old.
# With no login, as above, doctor gets none. With one, doctor keeps it.
: >"$tmp/agy-bin.seen"
real_path_agy=$tmp/agy-signed-in run_real status >/dev/null 2>&1 || true
expect_grep "^$tmp/agy-signed-in\$" "$tmp/agy-bin.seen" "a signed-in agy stays for doctor on the real path"

echo "rehearsal: all checks passed"
