#!/bin/bash
# Rehearse sign-release.sh end-to-end on a fake toolchain. gh, codesign,
# spctl, xcrun, ditto and sleep come first in PATH and only record their
# calls, so the real control flow — checksum verify, package-version check,
# ad-hoc and Developer ID signing, the hardened-runtime and team checks,
# the notary submit/wait loop, repacking, and the upload/edit tail — runs
# with no keychain, no Apple, and no network. Nothing here can publish:
# gh is a fake that writes into a call log.
#
#   scripts/sign-release-rehearsal.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
sign=$here/sign-release.sh

tmp=$(mktemp -d "${TMPDIR:-/tmp}/sign-release-rehearsal.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
export SR_TMP=$tmp

tools=$tmp/tools
mkdir -p "$tools" "$tmp/download" "$tmp/run"

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
    if ! grep -q "$1" "$2" 2>/dev/null; then
        echo "rehearsal: FAIL: $3 — no '$1' in $2:" >&2
        cat "$2" >&2 2>/dev/null || true
        exit 1
    fi
    echo "rehearsal: ok: $3"
}

expect_nogrep() { # expect_nogrep <pattern> <file> <what>
    if grep -q "$1" "$2" 2>/dev/null; then
        echo "rehearsal: FAIL: $3 — '$1' in $2:" >&2
        cat "$2" >&2
        exit 1
    fi
    echo "rehearsal: ok: $3"
}

# --- fake toolchain -----------------------------------------------------------
# Every fake appends its invocation to <tool>.calls; marker files under $tmp
# steer the failure paths: no-runtime, team-notset, fail-verify, fail-testreq.

cat >"$tools/gh" <<'EOF'
#!/bin/sh
echo "gh $*" >>"$SR_TMP/gh.calls"
case " $* " in
    *" release download "*)
        dir=
        prev=
        for a in "$@"; do
            [ "$prev" = -D ] && dir=$a
            prev=$a
        done
        cp "$SR_TMP"/download/* "$dir"/
        ;;
esac
exit 0
EOF

cat >"$tools/codesign" <<'EOF'
#!/bin/sh
echo "codesign $*" >>"$SR_TMP/codesign.calls"
prev= sign_id= arch= dv= verify= testreq=
for a in "$@"; do
    case $a in
        --sign) prev=sign ;;
        --arch) prev=arch ;;
        -dv) dv=1 ;;
        --verify) verify=1 ;;
        --test-requirement*) testreq=1 ;;
        *) case $prev in
               sign) sign_id=$a ;;
               arch) arch=$a ;;
           esac
           prev= ;;
    esac
done
if [ -n "$dv" ]; then
    if [ -f "$SR_TMP/no-runtime" ]; then
        echo 'flags=0x0(none)' >&2
    else
        echo 'flags=0x10000(runtime)' >&2
    fi
    if [ -z "$arch" ]; then
        if [ -f "$SR_TMP/team-notset" ]; then
            echo 'TeamIdentifier=not set' >&2
        else
            echo 'TeamIdentifier=ABCDE12345' >&2
        fi
    fi
    exit 0
fi
[ -n "$testreq" ] && [ -f "$SR_TMP/fail-testreq" ] && exit 1
[ -n "$verify" ] && [ -f "$SR_TMP/fail-verify" ] && exit 1
[ -n "$sign_id" ] && echo "$sign_id" >"$SR_TMP/sign.id"
exit 0
EOF

cat >"$tools/spctl" <<'EOF'
#!/bin/sh
echo "spctl $*" >>"$SR_TMP/spctl.calls"
n=$(cat "$SR_TMP/spctl.n" 2>/dev/null || echo 0)
n=$((n + 1))
echo "$n" >"$SR_TMP/spctl.n"
mode=$(cat "$SR_TMP/spctl.mode" 2>/dev/null || echo always)
case $mode in
    later) [ "$n" -ge 2 ] && echo 'source=Notarized Developer ID' || echo 'source=Developer ID' ;;
    never) echo 'source=Developer ID' ;;
    *) echo 'source=Notarized Developer ID' ;;
esac
exit 0
EOF

cat >"$tools/xcrun" <<'EOF'
#!/bin/sh
echo "xcrun $*" >>"$SR_TMP/xcrun.calls"
case " $* " in
    *" notarytool submit "*) cat "$SR_TMP/notary.out" ;;
    *" notarytool log "*) echo 'statusLog: the binary uses an SDK too old' >&2 ;;
esac
exit "${SR_XCRUN_EXIT:-0}"
EOF

cat >"$tools/ditto" <<'EOF'
#!/bin/sh
echo "ditto $*" >>"$SR_TMP/ditto.calls"
for last; do :; done
: > "$last"
EOF

printf '#!/bin/sh\nexit 0\n' >"$tools/sleep"
chmod 755 "$tools"/*

# --- helpers ------------------------------------------------------------------

# mkpkg <version> — the package gh's fake download serves: a conbus whose
# --version answers <version>, plus the checksum publish.sh puts beside it.
mkpkg() {
    rm -rf "$tmp/pkg" "$tmp/download"
    mkdir -p "$tmp/pkg" "$tmp/download"
    printf '#!/bin/sh\n[ "$1" = --version ] && echo "conbus %s (f4k3)"\n' "$1" \
        >"$tmp/pkg/conbus"
    chmod 755 "$tmp/pkg/conbus"
    (cd "$tmp/pkg" && tar -czf "$tmp/download/conbus-macos-universal.tar.gz" conbus)
    (cd "$tmp/download" &&
        shasum -a 256 conbus-macos-universal.tar.gz >conbus-macos-universal.tar.gz.sha256)
}

# run_sign <identity> <args...> — target/signed lands under $tmp/run.
run_sign() {
    identity=$1
    shift
    (cd "$tmp/run" && env PATH="$tools:$PATH" \
        CONBUS_SIGN_IDENTITY="$identity" SR_TMP="$tmp" \
        sh "$sign" "$@")
}

reset_markers() {
    rm -f "$tmp"/{gh,codesign,spctl,xcrun,ditto}.calls "$tmp"/{sign.id,spctl.n,spctl.mode,notary.out} \
        "$tmp"/{no-runtime,team-notset,fail-verify,fail-testreq}
    rm -rf "$tmp/run/target"
}

devid="Developer ID Application: Deximple (ABCDE12345)"

# --- validation ---------------------------------------------------------------

code=0
run_sign - not-a-tag >"$tmp/t1.out" 2>&1 || code=$?
expect 1 "$code" "a malformed tag is refused"
expect_grep 'usage:' "$tmp/t1.out" "the usage line says so"

code=0
env PATH="$tools:$PATH" sh "$sign" v9.9.9 >"$tmp/t2.out" 2>&1 || code=$?
expect 1 "$code" "no CONBUS_SIGN_IDENTITY is refused"
expect_grep 'CONBUS_SIGN_IDENTITY' "$tmp/t2.out" "the missing identity is named"

code=0
run_sign - v9.9.9 --upload >"$tmp/t3.out" 2>&1 || code=$?
expect 1 "$code" "a rehearsal may not --upload"
expect_grep 'never uploaded' "$tmp/t3.out" "it says why"

code=0
run_sign - v9.9.9 --bogus >"$tmp/t4.out" 2>&1 || code=$?
expect 1 "$code" "an unknown second argument is refused"
expect_grep 'only option is --upload' "$tmp/t4.out" "the option is named"

# --- the published package is checked before anything is signed ---------------

mkpkg 9.9.9
awk '{print "0000000000000000000000000000000000000000000000000000000000000000  " $2}' \
    "$tmp/download/conbus-macos-universal.tar.gz.sha256" \
    >"$tmp/download/bad" && mv "$tmp/download/bad" \
    "$tmp/download/conbus-macos-universal.tar.gz.sha256"
code=0
run_sign - v9.9.9 >"$tmp/t5.out" 2>&1 || code=$?
expect 1 "$code" "a package that fails its checksum dies"
expect_grep 'does not match' "$tmp/t5.out" "the checksum mismatch is named"
expect_absent "$tmp/sign.id" "nothing was signed"
reset_markers

mkpkg 9.9.8 # the download serves 9.9.8; the tag asks for 9.9.9
code=0
run_sign - v9.9.9 >"$tmp/t6.out" 2>&1 || code=$?
expect 1 "$code" "a package whose version is not the tag dies"
expect_grep 'conbus 9.9.8, not v9.9.9' "$tmp/t6.out" "both versions are named"
expect_absent "$tmp/sign.id" "nothing was signed"
reset_markers

# --- rehearsal path: ad-hoc sign, no Apple ------------------------------------

mkpkg 9.9.9
code=0
run_sign - v9.9.9 >"$tmp/t7.out" 2>&1 || code=$?
expect 0 "$code" "the ad-hoc rehearsal completes"
expect "-" "$(cat "$tmp/sign.id")" "it signed ad hoc"
expect_file "$tmp/run/target/signed/conbus-macos-universal.tar.gz" "the signed package exists"
expect_grep 'release download v9.9.9' "$tmp/gh.calls" "the published package was fetched"
expect_nogrep 'release upload' "$tmp/gh.calls" "no upload without --upload"
expect_absent "$tmp/xcrun.calls" "a rehearsal never talks to Apple"
expect "$(tar -tzf "$tmp/download/conbus-macos-universal.tar.gz")" \
    "$(tar -tzf "$tmp/run/target/signed/conbus-macos-universal.tar.gz")" \
    "the signed package keeps the same members"
reset_markers

# --- signature checks that must fail ------------------------------------------

touch "$tmp/no-runtime"
code=0
run_sign - v9.9.9 >"$tmp/t8.out" 2>&1 || code=$?
expect 1 "$code" "a signature without the hardened runtime dies"
expect_grep 'not signed with the hardened runtime' "$tmp/t8.out" "the slice is named"
reset_markers

touch "$tmp/fail-verify"
code=0
run_sign - v9.9.9 >"$tmp/t9.out" 2>&1 || code=$?
expect 1 "$code" "a signature that does not verify dies"
expect_grep 'does not verify' "$tmp/t9.out" "the verify failure is named"
reset_markers

# --- Developer ID path --------------------------------------------------------

# Already notarized: spctl says so on the first look, so Apple is never asked.
echo always >"$tmp/spctl.mode"
code=0
run_sign "$devid" v9.9.9 >"$tmp/t10.out" 2>&1 || code=$?
expect 0 "$code" "an already-notarized binary completes"
expect "$devid" "$(cat "$tmp/sign.id")" "it signed with the Developer ID"
expect_grep 'already notarized' "$tmp/t10.out" "it says it skipped the submission"
expect_absent "$tmp/xcrun.calls" "notarytool was never invoked"
expect_grep 'run again with --upload' "$tmp/t10.out" "without --upload it stops after signing"
reset_markers

# Needs submission: spctl votes notarized only from the second look, so the
# submit runs and the publish-ticket wait exits on the next assessment.
echo later >"$tmp/spctl.mode"
printf '{"status":"Accepted","id":"abc-1"}\n' >"$tmp/notary.out"
code=0
run_sign "$devid" v9.9.9 >"$tmp/t11.out" 2>&1 || code=$?
expect 0 "$code" "a fresh notarization completes"
expect_grep 'notarytool submit' "$tmp/xcrun.calls" "the zip went to Apple"
expect_file "$tmp/run/target/signed/conbus-macos-universal.tar.gz" "the signed package exists"
expect 2 "$(cat "$tmp/spctl.n")" "the ticket wait polled until notarized"
reset_markers

# Accepted but the ticket never lands: the wait gives up with the rerun hint.
echo never >"$tmp/spctl.mode"
printf '{"status":"Accepted","id":"abc-1"}\n' >"$tmp/notary.out"
code=0
run_sign "$devid" v9.9.9 >"$tmp/t12.out" 2>&1 || code=$?
expect 1 "$code" "a ticket that never publishes gives up"
expect_grep 'run again later' "$tmp/t12.out" "it says the ticket will be found"
reset_markers

# Rejected: the notary log is fetched for the operator, then it dies.
echo never >"$tmp/spctl.mode"
printf '{"status":"Invalid","id":"deadbeef-1"}\n' >"$tmp/notary.out"
code=0
run_sign "$devid" v9.9.9 >"$tmp/t13.out" 2>&1 || code=$?
expect 1 "$code" "a notary rejection dies"
expect_grep 'did not accept' "$tmp/t13.out" "the rejection is named"
expect_grep 'notarytool log deadbeef-1' "$tmp/xcrun.calls" "the rejection's log was fetched"
expect_absent "$tmp/run/target" "nothing was repacked"
reset_markers

# A signature that names no team is not a Developer ID.
touch "$tmp/team-notset"
code=0
run_sign "$devid" v9.9.9 >"$tmp/t14.out" 2>&1 || code=$?
expect 1 "$code" "a signature with no team dies"
expect_grep 'not a Developer ID' "$tmp/t14.out" "the team check is named"
reset_markers

# --- upload -------------------------------------------------------------------

echo always >"$tmp/spctl.mode"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t15.out" 2>&1 || code=$?
expect 0 "$code" "a release tag uploads"
expect_grep 'release upload v9.9.9' "$tmp/gh.calls" "the signed package was uploaded"
expect_grep 'release edit v9.9.9 --draft=false --latest' "$tmp/gh.calls" \
    "a release tag becomes latest exactly once"
reset_markers

mkpkg 9.9.9-rc.1
echo always >"$tmp/spctl.mode"
code=0
run_sign "$devid" v9.9.9-rc.1 --upload >"$tmp/t16.out" 2>&1 || code=$?
expect 0 "$code" "an rc tag uploads"
expect_grep 'release upload v9.9.9-rc.1' "$tmp/gh.calls" "the rc package was uploaded"
expect_nogrep 'release edit' "$tmp/gh.calls" "an rc never becomes latest"
reset_markers

echo "rehearsal: all checks passed"
