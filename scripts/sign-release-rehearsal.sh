#!/bin/bash
# Rehearse sign-release.sh end-to-end on a fake toolchain. gh, codesign, spctl,
# xcrun, ditto and sleep come first in PATH, so the real control flow runs with
# no keychain, no Apple and no network. Nothing here can publish: gh is a fake
# that keeps a release in a directory, and curl a fake that serves it.
#
# The fake gh keeps state: whether the release is a draft, the bytes of each
# asset, what they were at the moment of publishing, and the order of everything
# that happened. So a check here asks what the release ended up as, not whether a
# command line went by. It behaves as gh 2.101 does where the script leans on it:
# an upload over an existing asset fails without --clobber, --clobber deletes the
# asset first, so a failed upload leaves it gone, and a download fails only when no
# pattern matches. A real draft has never been through any of this.
#
#   scripts/sign-release-rehearsal.sh
set -euo pipefail

here=$(cd "$(dirname "$0")" && pwd)
sign=$here/sign-release.sh
install_sh=$here/../install.sh
workflow=$here/../.github/workflows/release.yml

tmp=$(mktemp -d "${TMPDIR:-/tmp}/sign-release-rehearsal.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
export SR_TMP=$tmp

tools=$tmp/tools
mkdir -p "$tools" "$tmp/run"

fail() {
    echo "rehearsal: FAIL: $*" >&2
    exit 1
}

expect() { # expect <want> <got> <what>
    [ "$1" = "$2" ] || fail "$3 — want $1, got $2"
    echo "rehearsal: ok: $3"
}

expect_file() {
    [ -f "$1" ] || fail "$2"
    echo "rehearsal: ok: $2"
}

expect_absent() {
    [ ! -e "$1" ] || fail "$2"
    echo "rehearsal: ok: $2"
}

expect_grep() { # expect_grep <pattern> <file> <what>
    if ! grep -q -- "$1" "$2" 2>/dev/null; then
        cat "$2" >&2 2>/dev/null || true
        fail "$3 — no '$1' in $2"
    fi
    echo "rehearsal: ok: $3"
}

expect_nogrep() { # expect_nogrep <pattern> <file> <what>
    if grep -q -- "$1" "$2" 2>/dev/null; then
        cat "$2" >&2
        fail "$3 — '$1' in $2"
    fi
    echo "rehearsal: ok: $3"
}

# --- fake toolchain -----------------------------------------------------------
# Every fake appends its invocation to <tool>.calls, and what matters to the
# release's story to order.log. Marker files under $tmp steer the failure paths.

cat >"$tools/gh" <<'EOF'
#!/bin/sh
echo "gh $*" >>"$SR_TMP/gh.calls"
release=$SR_TMP/release
[ "$1" = release ] || { echo "fake gh: only releases are rehearsed: $*" >&2; exit 2; }
sub=$2
# With no tag, `gh release view` asks about the latest release.
if [ "$sub" = view ] && [ "$*" = "release view --json tagName --jq .tagName" ]; then
    if [ -f "$SR_TMP/latest-is-another" ]; then
        echo v0.0.1
    elif [ -f "$release/latest" ]; then
        cat "$release/tag"
    else
        echo v0.0.1
    fi
    exit 0
fi
tag=$3
shift 3
[ "$tag" = "$(cat "$release/tag")" ] || { echo "release not found" >&2; exit 1; }
case $sub in
    view)
        [ "$*" = "--json isDraft --jq .isDraft" ] || { echo "fake gh: view $*" >&2; exit 2; }
        cat "$release/draft"
        ;;
    download)
        patterns=
        dir=
        while [ $# -gt 0 ]; do
            case $1 in
                -p) patterns="$patterns $2"; shift 2 ;;
                -D) dir=$2; shift 2 ;;
                *) echo "fake gh: download flag $1" >&2; exit 2 ;;
            esac
        done
        echo "download$patterns" >>"$SR_TMP/order.log"
        found=
        for pattern in $patterns; do
            if [ -f "$release/assets/$pattern" ]; then
                cp "$release/assets/$pattern" "$dir/"
                found=1
            fi
        done
        [ -n "$found" ] || { echo "no assets match the file pattern" >&2; exit 1; }
        ;;
    upload)
        clobber=
        files=
        for arg in "$@"; do
            case $arg in
                --clobber) clobber=1 ;;
                -*) echo "fake gh: upload flag $arg" >&2; exit 2 ;;
                *) files="$files $arg" ;;
            esac
        done
        # gh deletes what it replaces before it uploads, so a failure leaves it gone.
        if [ -f "$SR_TMP/fail-upload" ]; then
            [ -z "$clobber" ] || for file in $files; do rm -f "$release/assets/$(basename "$file")"; done
            echo "upload failed" >&2
            exit 1
        fi
        for file in $files; do
            name=$(basename "$file")
            if [ -f "$release/assets/$name" ] && [ -z "$clobber" ]; then
                echo "asset under the same name already exists: [$name]" >&2
                exit 1
            fi
        done
        for file in $files; do
            name=$(basename "$file")
            echo "upload $name" >>"$SR_TMP/order.log"
            [ -f "$SR_TMP/upload-lost" ] || cp "$file" "$release/assets/$name"
        done
        # Another job's clobber took an asset away just as this one finished.
        if [ -f "$SR_TMP/racing-job-takes" ]; then
            rm -f "$release/assets/$(cat "$SR_TMP/racing-job-takes")"
        fi
        ;;
    edit)
        [ ! -f "$SR_TMP/fail-edit" ] || { echo "edit failed" >&2; exit 1; }
        latest=
        draft=
        for arg in "$@"; do
            case $arg in
                --latest) latest=1 ;;
                --draft=false) draft=false ;;
                *) echo "fake gh: edit flag $arg" >&2; exit 2 ;;
            esac
        done
        [ "$draft" = false ] || { echo "fake gh: an edit that does not publish" >&2; exit 2; }
        echo publish >>"$SR_TMP/order.log"
        echo false >"$release/draft"
        [ -z "$latest" ] || echo true >"$release/latest"
        n=$(cat "$release/publishes" 2>/dev/null || echo 0)
        echo $((n + 1)) >"$release/publishes"
        # What people got at that moment, whatever happens afterwards.
        [ -d "$release/published" ] || cp -R "$release/assets" "$release/published"
        ;;
    *) echo "fake gh: release $sub" >&2; exit 2 ;;
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
    last=$a
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
            echo "TeamIdentifier=$(cat "$SR_TMP/team")" >&2
        fi
    fi
    exit 0
fi
if [ -n "$testreq" ]; then
    echo requirement >>"$SR_TMP/order.log"
    [ ! -f "$SR_TMP/fail-testreq" ] || exit 1
    exit 0
fi
[ -n "$verify" ] && [ -f "$SR_TMP/fail-verify" ] && exit 1
if [ -n "$sign_id" ]; then
    echo "$sign_id" >"$SR_TMP/sign.id"
    echo sign >>"$SR_TMP/order.log"
    # The signature travels in the bytes, so a package can be asked what it holds.
    printf '# signed by %s\n' "$sign_id" >>"$last"
    if [ -f "$SR_TMP/break-binary" ]; then
        printf '#!/bin/sh\nexit 1\n' >"$last"
    fi
fi
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
    *" notarytool submit "*)
        echo submit >>"$SR_TMP/order.log"
        cat "$SR_TMP/notary.out"
        [ ! -f "$SR_TMP/fail-submit" ] || exit 1
        ;;
    *" notarytool log "*) echo 'statusLog: the binary uses an SDK too old' >&2 ;;
esac
exit 0
EOF

cat >"$tools/ditto" <<'EOF'
#!/bin/sh
echo "ditto $*" >>"$SR_TMP/ditto.calls"
for last; do :; done
: > "$last"
EOF

# An anonymous download: only a published latest release is served, as GitHub does.
cat >"$tools/curl" <<'EOF'
#!/bin/sh
echo "curl $*" >>"$SR_TMP/curl.calls"
echo curl >>"$SR_TMP/order.log"
release=$SR_TMP/release
url=
out=
while [ $# -gt 0 ]; do
    case $1 in
        -o) out=$2; shift 2 ;;
        -*) shift ;;
        *) url=$1; shift ;;
    esac
done
name=${url##*/}
case $url in
    https://github.com/*/releases/latest/download/*) ;;
    *) echo "fake curl: $url" >&2; exit 22 ;;
esac
[ "$(cat "$release/draft")" = false ] || exit 22
[ -f "$release/latest" ] || exit 22
if [ -f "$SR_TMP/curl-slow-start" ]; then
    n=$(cat "$SR_TMP/curl.n" 2>/dev/null || echo 0)
    echo $((n + 1)) >"$SR_TMP/curl.n"
    [ "$n" -ge 3 ] || exit 22
fi
[ ! -f "$SR_TMP/curl-404" ] || exit 22
if [ -f "$SR_TMP/curl-stale" ]; then
    printf 'an older copy\n' >"$out"
    exit 0
fi
cp "$release/assets/$name" "$out"
EOF

# For package.sh on a Mac: joins nothing, and says nothing of interest.
cat >"$tools/lipo" <<'EOF'
#!/bin/sh
case $1 in
    -create)
        cp "$4" "$3"
        ;;
esac
exit 0
EOF

printf '#!/bin/sh\nexit 0\n' >"$tools/sleep"
chmod 755 "$tools"/*

# --- the release --------------------------------------------------------------

# What an install fetches is install.sh's to say, so the list comes from there: a
# platform added to it and not to sign-release.sh fails the refusals below.
packages=$(sed -n 's/.*asset=\(conbus-[a-z0-9_-]*\.tar\.gz\).*/\1/p' "$install_sh")
expect 3 "$(printf '%s\n' "$packages" | wc -l | tr -d ' ')" "install.sh names three packages"
required=install.sh
for package in $packages; do
    required="$required $package $package.sha256"
done
macos=conbus-macos-universal.tar.gz

# The team install.sh refuses every other signature without, read from there.
team_required=$(sed -n 's/.*\[ "$team" = \([A-Z0-9]*\) \].*/\1/p' "$install_sh")
[ -n "$team_required" ] || fail "install.sh names no team"

# A stand-in conbus: its --version, and the stamp build_info.rs puts in a real one.
binary_of_version() {
    printf '#!/bin/sh\n# conbus-version %s;\n[ "$1" = --version ] && echo "conbus %s (f4k3)"\nexit 0\n' \
        "$1" "$1"
}

# seed_release <package version> <tag> <draft: true|false> — the release as the
# workflow leaves it: ad-hoc packages, each with its checksum, and install.sh.
seed_release() {
    rm -rf "$tmp/pkg" "$tmp/release" "$tmp/seed"
    mkdir -p "$tmp/pkg" "$tmp/release/assets"
    printf '%s\n' "$2" >"$tmp/release/tag"
    printf '%s\n' "$3" >"$tmp/release/draft"
    binary_of_version "$1" >"$tmp/pkg/conbus"
    chmod 755 "$tmp/pkg/conbus"
    printf 'the licence\n' >"$tmp/pkg/LICENSE"
    for package in $packages; do
        (cd "$tmp/pkg" && tar -czf "$tmp/release/assets/$package" conbus LICENSE)
        (cd "$tmp/release/assets" && shasum -a 256 "$package" >"$package.sha256")
    done
    cp "$install_sh" "$tmp/release/assets/install.sh"
    cp -R "$tmp/release/assets" "$tmp/seed"
}

# run_sign <identity> <args...> — target/signed lands under $tmp/run.
run_sign() {
    identity=$1
    shift
    (cd "$tmp/run" && env PATH="$tools:$PATH" \
        CONBUS_SIGN_IDENTITY="$identity" SR_TMP="$tmp" \
        sh "$sign" "$@")
}

# scenario <package version> <tag> <draft> — a clean slate and a fresh release.
scenario() {
    rm -f "$tmp"/{gh,codesign,spctl,xcrun,ditto}.calls "$tmp"/{sign.id,spctl.n,spctl.mode,notary.out} \
        "$tmp"/{no-runtime,team-notset,fail-verify,fail-testreq,fail-submit,break-binary} \
        "$tmp"/{fail-upload,fail-edit,upload-lost,racing-job-takes} "$tmp/order.log" \
        "$tmp"/{curl.calls,curl.n,curl-404,curl-stale,curl-slow-start,latest-is-another}
    : >"$tmp/order.log"
    echo "$team_required" >"$tmp/team"
    rm -rf "$tmp/run/target"
    seed_release "$1" "$2" "$3"
}

# The release is what the workflow left, and it is still what it was.
expect_untouched() { # expect_untouched <draft: true|false> <what>
    diff -r "$tmp/seed" "$tmp/release/assets" >/dev/null || fail "$2: the assets changed"
    expect "$1" "$(cat "$tmp/release/draft")" "$2: the release stays as it was"
    expect_absent "$tmp/release/publishes" "$2: nothing was published"
}

# The signature the package <file> holds, if any.
signature_in() {
    tar -xOzf "$1" conbus | sed -n 's/^# signed by //p'
}

# Line of the first or last event in order.log starting with $2.
order_of() { # order_of <first|last> <event>
    local lines
    lines=$(grep -n "^$2" "$tmp/order.log" | cut -d: -f1) || true
    [ -n "$lines" ] || fail "no $2 in the order of events: $(tr '\n' ',' <"$tmp/order.log")"
    if [ "$1" = first ]; then head -1 <<<"$lines"; else tail -1 <<<"$lines"; fi
}

expect_order() { # expect_order <line a> <line b> <what>
    [ "$1" -lt "$2" ] || fail "$3: $(tr '\n' ',' <"$tmp/order.log")"
    echo "rehearsal: ok: $3"
}

devid="Developer ID Application: Deximple ($team_required)"

# A package's owner and group, as tar lists them: `root/root`, whichever tar it is.
owners_of() {
    tar -tvzf "$1" | awk '{ if ($2 ~ /\//) print $2; else print $3 "/" $4 }' | sort -u
}

# Put a package on the release in place of the seeded one, with a true checksum.
replace_package() { # replace_package <name> <version in its binary>
    rm -rf "$tmp/other"
    mkdir -p "$tmp/other"
    binary_of_version "$2" >"$tmp/other/conbus"
    chmod 755 "$tmp/other/conbus"
    (cd "$tmp/other" && tar -czf "$tmp/release/assets/$1" conbus)
    (cd "$tmp/release/assets" && shasum -a 256 "$1" >"$1.sha256")
    cp "$tmp/release/assets/$1" "$tmp/release/assets/$1.sha256" "$tmp/seed/"
}

# --- validation ---------------------------------------------------------------

scenario 9.9.9 v9.9.9 true
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

scenario 9.9.9 v9.9.9 true
sum=$tmp/release/assets/$macos.sha256
awk '{print "0000000000000000000000000000000000000000000000000000000000000000  " $2}' \
    "$sum" >"$sum.bad" && mv "$sum.bad" "$sum"
code=0
run_sign - v9.9.9 >"$tmp/t5.out" 2>&1 || code=$?
expect 1 "$code" "a package that fails its checksum dies"
expect_grep 'does not match' "$tmp/t5.out" "the checksum mismatch is named"
expect_absent "$tmp/sign.id" "nothing was signed"

scenario 9.9.8 v9.9.9 true # the release holds 9.9.8; the tag asks for 9.9.9
code=0
run_sign - v9.9.9 >"$tmp/t6.out" 2>&1 || code=$?
expect 1 "$code" "a package whose version is not the tag dies"
expect_grep 'conbus 9.9.8, not v9.9.9' "$tmp/t6.out" "both versions are named"
expect_absent "$tmp/sign.id" "nothing was signed"

# --- rehearsal path: ad-hoc sign, no Apple ------------------------------------

scenario 9.9.9 v9.9.9 true
code=0
run_sign - v9.9.9 >"$tmp/t7.out" 2>&1 || code=$?
expect 0 "$code" "the ad-hoc rehearsal completes"
expect "-" "$(cat "$tmp/sign.id")" "it signed ad hoc"
expect_file "$tmp/run/target/signed/$macos" "the signed package exists"
expect "-" "$(signature_in "$tmp/run/target/signed/$macos")" "the package holds that signature"
expect_grep 'release download v9.9.9' "$tmp/gh.calls" "the published package was fetched"
expect_untouched true "an ad-hoc rehearsal"
expect_absent "$tmp/xcrun.calls" "a rehearsal never talks to Apple"
expect "$(tar -tzf "$tmp/seed/$macos")" \
    "$(tar -tzf "$tmp/run/target/signed/$macos")" \
    "the signed package keeps the same members"
expect root/root "$(owners_of "$tmp/run/target/signed/$macos")" \
    "the signed package names no account in its tar headers"

# --- signature checks that must fail ------------------------------------------

scenario 9.9.9 v9.9.9 true
touch "$tmp/no-runtime"
code=0
run_sign - v9.9.9 >"$tmp/t8.out" 2>&1 || code=$?
expect 1 "$code" "a signature without the hardened runtime dies"
expect_grep 'not signed with the hardened runtime' "$tmp/t8.out" "the slice is named"

scenario 9.9.9 v9.9.9 true
touch "$tmp/fail-verify"
code=0
run_sign - v9.9.9 >"$tmp/t9.out" 2>&1 || code=$?
expect 1 "$code" "a signature that does not verify dies"
expect_grep 'does not verify' "$tmp/t9.out" "the verify failure is named"

# --- Developer ID path, signing only ------------------------------------------

# Already notarized: spctl says so on the first look, so Apple is never asked.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
code=0
run_sign "$devid" v9.9.9 >"$tmp/t10.out" 2>&1 || code=$?
expect 0 "$code" "an already-notarized binary completes"
expect "$devid" "$(cat "$tmp/sign.id")" "it signed with the Developer ID"
expect_grep 'already notarized' "$tmp/t10.out" "it says it skipped the submission"
expect_absent "$tmp/xcrun.calls" "notarytool was never invoked"
expect_grep 'run again with --upload' "$tmp/t10.out" "without --upload it stops after signing"
expect_untouched true "signing without --upload"

# Signing alone needs only the macOS package: the Linux jobs may still be running.
scenario 9.9.9 v9.9.9 true
rm -f "$tmp/release/assets/conbus-linux-aarch64.tar.gz" "$tmp/seed/conbus-linux-aarch64.tar.gz"
echo always >"$tmp/spctl.mode"
code=0
run_sign "$devid" v9.9.9 >"$tmp/t10b.out" 2>&1 || code=$?
expect 0 "$code" "signing without --upload does not need the Linux packages"

# Needs submission: spctl votes notarized only from the second look, so the
# submit runs and the publish-ticket wait exits on the next assessment.
scenario 9.9.9 v9.9.9 true
echo later >"$tmp/spctl.mode"
printf '{"status":"Accepted","id":"abc-1"}\n' >"$tmp/notary.out"
code=0
run_sign "$devid" v9.9.9 >"$tmp/t11.out" 2>&1 || code=$?
expect 0 "$code" "a fresh notarization completes"
expect_grep 'notarytool submit' "$tmp/xcrun.calls" "the zip went to Apple"
expect_file "$tmp/run/target/signed/$macos" "the signed package exists"
expect 2 "$(cat "$tmp/spctl.n")" "the ticket wait polled until notarized"

# --- every way the signing can fail, with --upload: the draft is left alone ---

# Accepted but the ticket never lands: the wait gives up with the rerun hint.
scenario 9.9.9 v9.9.9 true
echo never >"$tmp/spctl.mode"
printf '{"status":"Accepted","id":"abc-1"}\n' >"$tmp/notary.out"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t12.out" 2>&1 || code=$?
expect 1 "$code" "a ticket that never publishes gives up"
expect_grep 'run again later' "$tmp/t12.out" "it says the ticket will be found"
expect_untouched true "a ticket that never lands"

# Rejected: the notary log is fetched for the operator, then it dies.
scenario 9.9.9 v9.9.9 true
echo never >"$tmp/spctl.mode"
printf '{"status":"Invalid","id":"deadbeef-1"}\n' >"$tmp/notary.out"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t13.out" 2>&1 || code=$?
expect 1 "$code" "a notary rejection dies"
expect_grep 'did not accept' "$tmp/t13.out" "the rejection is named"
expect_grep 'notarytool log deadbeef-1' "$tmp/xcrun.calls" "the rejection's log was fetched"
expect_absent "$tmp/run/target" "nothing was repacked"
expect_untouched true "a notary rejection"

# The submit itself fails, whatever it printed: an answer from a failed submit
# is not an answer.
scenario 9.9.9 v9.9.9 true
echo later >"$tmp/spctl.mode"
printf '{"status":"Accepted","id":"abc-1"}\n' >"$tmp/notary.out"
touch "$tmp/fail-submit"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t13b.out" 2>&1 || code=$?
expect 1 "$code" "a failed submit dies"
expect_grep 'could not submit' "$tmp/t13b.out" "the failed submit is named"
expect_untouched true "a failed submit"

# Gatekeeper agrees, the notarized requirement does not.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
touch "$tmp/fail-testreq"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t13c.out" 2>&1 || code=$?
expect 1 "$code" "a binary that misses the notarized requirement dies"
expect_grep 'notarized requirement' "$tmp/t13c.out" "the requirement is named"
expect_untouched true "a missed requirement"

# The signature broke the binary.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
touch "$tmp/break-binary"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t13d.out" 2>&1 || code=$?
expect 1 "$code" "a signed binary that does not run dies"
expect_grep 'does not run' "$tmp/t13d.out" "it says the binary does not run"
expect_untouched true "a binary that does not run"

# A signature that names no team is not a Developer ID.
scenario 9.9.9 v9.9.9 true
touch "$tmp/team-notset"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t14.out" 2>&1 || code=$?
expect 1 "$code" "a signature with no team dies"
expect_grep 'not a Developer ID' "$tmp/t14.out" "the team check is named"
expect_untouched true "a signature with no team"

# --- upload and publish -------------------------------------------------------

scenario 9.9.9 v9.9.9 true
echo later >"$tmp/spctl.mode"
printf '{"status":"Accepted","id":"abc-1"}\n' >"$tmp/notary.out"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t15.out" 2>&1 || code=$?
expect 0 "$code" "a release tag uploads and publishes"
expect false "$(cat "$tmp/release/draft")" "the draft is published"
expect true "$(cat "$tmp/release/latest")" "it is the latest release"
expect 1 "$(cat "$tmp/release/publishes")" "it is published exactly once"
expect "$devid" "$(signature_in "$tmp/release/published/$macos")" \
    "what was public at that moment holds the Developer ID signature"
(cd "$tmp/release/published" && shasum -a 256 -c "$macos.sha256" >/dev/null) ||
    fail "the published checksum does not match the published package"
echo "rehearsal: ok: the published checksum matches the published package"
for name in $required; do
    expect_file "$tmp/release/published/$name" "$name was on the release when it went public"
done
for name in $required; do
    case $name in
        "$macos" | "$macos.sha256") ;;
        *) cmp -s "$tmp/seed/$name" "$tmp/release/assets/$name" ||
            fail "$name changed: only the macOS package is this script's to replace" ;;
    esac
done
echo "rehearsal: ok: only the macOS package was replaced"
expect 2 "$(grep -c '^upload' "$tmp/order.log")" "the package and its checksum went up once each"
expect_order "$(order_of first sign)" "$(order_of first submit)" "signed before it went to Apple"
expect_order "$(order_of first submit)" "$(order_of first requirement)" \
    "the notarized requirement is checked after Apple answered"
expect_order "$(order_of last requirement)" "$(order_of first upload)" \
    "uploaded only after every check passed"
expect_order "$(order_of last upload)" "$(order_of last download)" \
    "the release is read back after the upload"
expect_order "$(order_of last download)" "$(order_of first publish)" \
    "published only after it was read back"
expect_order "$(order_of first publish)" "$(order_of first curl)" \
    "what people get is fetched after publishing"
for name in $required; do
    expect_grep "releases/latest/download/$name -o" "$tmp/curl.calls" "$name is fetched as an install fetches it"
done
expect_grep 'is published and is now the latest release' "$tmp/t15.out" "it says it is published"
expect_nogrep 'IS PUBLISHED, AND' "$tmp/t15.out" "and raises no alarm"
expect root/root "$(owners_of "$tmp/release/assets/$macos")" \
    "the package people get names no account in its tar headers"

# A failed publish leaves a draft and says so through gh's own error.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
touch "$tmp/fail-edit"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t15b.out" 2>&1 || code=$?
expect 1 "$code" "a publish that fails fails the run"
expect true "$(cat "$tmp/release/draft")" "a failed publish leaves the draft"
expect_grep 'publishing it failed' "$tmp/t15b.out" "a failed publish says so"
expect_grep 'run this again with --upload' "$tmp/t15b.out" "a failed publish says what to do"

# An upload that fails stops before any publish. gh deleted what it was replacing,
# and this first run has to say so: the next one would only find the assets missing.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
touch "$tmp/fail-upload"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t15c.out" 2>&1 || code=$?
expect 1 "$code" "an upload that fails fails the run"
expect true "$(cat "$tmp/release/draft")" "a failed upload leaves the draft"
expect_absent "$tmp/release/publishes" "a failed upload publishes nothing"
expect_absent "$tmp/release/assets/$macos" "gh deleted the package before it failed"
expect_file "$tmp/release/assets/conbus-linux-x86_64.tar.gz" "the Linux packages were not touched"
expect_grep 'stays a draft' "$tmp/t15c.out" "a failed upload says the release stays a draft"
expect_grep 'the upload failed' "$tmp/t15c.out" "a failed upload says so on its first run"
expect_grep 'may now be gone' "$tmp/t15c.out" "a failed upload says what it left behind"
expect_grep 'gh release upload v9.9.9 target/signed' "$tmp/t15c.out" "a failed upload gives the hand upload"
expect_file "$tmp/run/target/signed/$macos" "the signed package is there for the hand upload"

# The hand upload it gives, then the same command again: that publishes.
rm -f "$tmp/fail-upload"
(cd "$tmp/run" && env PATH="$tools:$PATH" SR_TMP="$tmp" gh release upload v9.9.9 \
    "target/signed/$macos" "target/signed/$macos.sha256" --clobber)
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t15e.out" 2>&1 || code=$?
expect 0 "$code" "the hand upload, then the same command, publishes"
expect 1 "$(cat "$tmp/release/publishes")" "and publishes once"

# A failed upload on a release candidate, public as it is, says what it stays as.
scenario 9.9.9-rc.1 v9.9.9-rc.1 false
echo always >"$tmp/spctl.mode"
touch "$tmp/fail-upload"
code=0
run_sign "$devid" v9.9.9-rc.1 --upload >"$tmp/t15f.out" 2>&1 || code=$?
expect 1 "$code" "an rc upload that fails fails the run"
expect_grep 'stays a prerelease' "$tmp/t15f.out" "and says what it stays"
expect_grep 'may now be gone' "$tmp/t15f.out" "and says what it left behind"

# gh says it uploaded, and the release still holds the ad-hoc package.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
touch "$tmp/upload-lost"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t15d.out" 2>&1 || code=$?
expect 1 "$code" "a release still holding the ad-hoc package is refused"
expect_grep 'not the signed one' "$tmp/t15d.out" "it says the package is not the signed one"
expect_grep 'stays a draft' "$tmp/t15d.out" "it says the release stays a draft"
expect_untouched true "an upload that did not land"

# --- a release that an install would 404 on is not published ------------------

# Every asset the install path needs, one at a time. Nothing is signed, because
# waiting on Apple for a release that cannot go out helps nobody.
for name in $required; do
    scenario 9.9.9 v9.9.9 true
    echo always >"$tmp/spctl.mode"
    rm -f "$tmp/release/assets/$name" "$tmp/seed/$name"
    code=0
    run_sign "$devid" v9.9.9 --upload >"$tmp/t17.out" 2>&1 || code=$?
    expect 1 "$code" "a release with no $name is refused"
    expect_grep "$name" "$tmp/t17.out" "the refusal names $name"
    expect_grep 'stays a draft' "$tmp/t17.out" "the refusal says it stays a draft"
    expect_grep 'gh run rerun' "$tmp/t17.out" "the refusal says what to do"
    expect_untouched true "a release with no $name"
    expect_absent "$tmp/sign.id" "nothing was signed for a release with no $name"
done

# Nothing at all on the release: gh refuses the download, and the refusal is still
# the script's own, with every name in it.
scenario 9.9.9 v9.9.9 true
rm -f "$tmp/release/assets"/* "$tmp/seed"/*
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t17b.out" 2>&1 || code=$?
expect 1 "$code" "a release with no assets is refused"
expect_grep 'conbus-linux-x86_64.tar.gz.sha256' "$tmp/t17b.out" "the refusal lists what is missing"
expect_untouched true "a release with no assets"

# An asset gone by the time it is read back: a Linux job's rerun clobbering it.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
echo conbus-linux-aarch64.tar.gz >"$tmp/racing-job-takes"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t18.out" 2>&1 || code=$?
expect 1 "$code" "an asset lost during the upload stops the publish"
expect_grep 'conbus-linux-aarch64.tar.gz' "$tmp/t18.out" "the refusal names it"
expect true "$(cat "$tmp/release/draft")" "it stays a draft"
expect_absent "$tmp/release/publishes" "nothing was published"

# A Linux package that does not match its checksum: every Linux install would
# refuse it.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
printf 'torn' >>"$tmp/release/assets/conbus-linux-x86_64.tar.gz"
cp "$tmp/release/assets/conbus-linux-x86_64.tar.gz" "$tmp/seed/"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t19.out" 2>&1 || code=$?
expect 1 "$code" "a Linux package that fails its checksum is refused"
expect_grep 'conbus-linux-x86_64.tar.gz does not match' "$tmp/t19.out" "the refusal names it"
expect_untouched true "a Linux package that fails its checksum"

# --- a release already public is not this script's to change ------------------

scenario 9.9.9 v9.9.9 false
echo always >"$tmp/spctl.mode"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t20.out" 2>&1 || code=$?
expect 1 "$code" "a release already published is refused"
expect_grep 'already published' "$tmp/t20.out" "it says so"
expect_absent "$tmp/sign.id" "nothing was signed"
expect "$(cat "$tmp/seed/$macos.sha256")" "$(cat "$tmp/release/assets/$macos.sha256")" \
    "the live package was not replaced"
expect_absent "$tmp/release/publishes" "an old release is not made latest again"

# --- a release candidate stays a prerelease -----------------------------------

scenario 9.9.9-rc.1 v9.9.9-rc.1 false
echo always >"$tmp/spctl.mode"
code=0
run_sign "$devid" v9.9.9-rc.1 --upload >"$tmp/t16.out" 2>&1 || code=$?
expect 0 "$code" "an rc tag uploads"
expect "$devid" "$(signature_in "$tmp/release/assets/$macos")" "the rc package holds the signature"
expect_absent "$tmp/release/publishes" "an rc is never published here"
expect_absent "$tmp/release/latest" "an rc never becomes latest"

scenario 9.9.9-rc.1 v9.9.9-rc.1 false
rm -f "$tmp/release/assets/conbus-linux-x86_64.tar.gz" "$tmp/seed/conbus-linux-x86_64.tar.gz"
code=0
run_sign "$devid" v9.9.9-rc.1 --upload >"$tmp/t16b.out" 2>&1 || code=$?
expect 1 "$code" "an rc with no Linux package is refused too"
expect_grep 'stays a prerelease' "$tmp/t16b.out" "the refusal says what it stays"
expect_untouched false "an rc with no Linux package"

# --- what is on it has to be what an install needs ------------------------------

# Signed by a team install.sh refuses: every macOS install of it would die.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
echo ABCDE12345 >"$tmp/team"
code=0
run_sign "Developer ID Application: Someone Else (ABCDE12345)" v9.9.9 --upload >"$tmp/t22.out" 2>&1 || code=$?
expect 1 "$code" "a signature from a team install.sh refuses is not published"
expect_grep "$team_required" "$tmp/t22.out" "the refusal names the team install.sh requires"
expect_grep 'ABCDE12345' "$tmp/t22.out" "the refusal names the team it got"
expect_untouched true "a signature from the wrong team"

# Without --upload too: finding out after Apple's review helps nobody.
scenario 9.9.9 v9.9.9 true
echo later >"$tmp/spctl.mode"
printf '{"status":"Accepted","id":"abc-1"}\n' >"$tmp/notary.out"
echo ABCDE12345 >"$tmp/team"
code=0
run_sign "Developer ID Application: Someone Else (ABCDE12345)" v9.9.9 >"$tmp/t22b.out" 2>&1 || code=$?
expect 1 "$code" "the wrong team is refused before anything goes to Apple"
expect_absent "$tmp/xcrun.calls" "nothing went to Apple"

# A checksum file that names another file checks nothing for its own package.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
cp "$tmp/release/assets/conbus-linux-x86_64.tar.gz.sha256" "$tmp/release/assets/conbus-linux-aarch64.tar.gz.sha256"
printf 'torn' >>"$tmp/release/assets/conbus-linux-aarch64.tar.gz"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t21.out" 2>&1 || code=$?
expect 1 "$code" "a checksum file naming another package is refused"
expect_absent "$tmp/release/publishes" "nothing was published"
expect_grep 'conbus-linux-aarch64.tar.gz.sha256 names conbus-linux-x86_64.tar.gz' "$tmp/t21.out" \
    "the refusal says which file it names"

# A checksum file holding two lines checks the second for nobody.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
cat "$tmp/release/assets/conbus-linux-x86_64.tar.gz.sha256" \
    >>"$tmp/release/assets/conbus-linux-aarch64.tar.gz.sha256"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t21b.out" 2>&1 || code=$?
expect 1 "$code" "a checksum file of two lines is refused"
expect_absent "$tmp/release/publishes" "nothing was published"

# Each package has to hold this version. A Mac cannot run a Linux binary, so the
# stamp is what is read, and 9.9.9 is inside 9.9.9-rc.1.
for carrying in $packages; do
    for wrong in 9.9.8 9.9.9-rc.1 19.9.9; do
        scenario 9.9.9 v9.9.9 true
        echo always >"$tmp/spctl.mode"
        replace_package "$carrying" "$wrong"
        code=0
        run_sign "$devid" v9.9.9 --upload >"$tmp/t23.out" 2>&1 || code=$?
        expect 1 "$code" "$carrying holding $wrong is refused for v9.9.9"
        expect_grep "$carrying holds a binary that is not v9.9.9" "$tmp/t23.out" "the refusal names it"
        expect_untouched true "$carrying holding $wrong"
    done
done

# A package with no binary in it at all.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
(cd "$tmp/pkg" && tar -czf "$tmp/release/assets/conbus-linux-x86_64.tar.gz" LICENSE)
(cd "$tmp/release/assets" && shasum -a 256 conbus-linux-x86_64.tar.gz >conbus-linux-x86_64.tar.gz.sha256)
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t23b.out" 2>&1 || code=$?
expect 1 "$code" "a package with no binary is refused"
expect_grep 'conbus-linux-x86_64.tar.gz holds' "$tmp/t23b.out" "the refusal names it"

# install.sh has to be the script: one byte is an asset, and it installs nothing.
for bad in 'x' '#!/bin/sh
echo hello
' '#!/bin/sh
if then
'; do
    scenario 9.9.9 v9.9.9 true
    echo always >"$tmp/spctl.mode"
    printf '%s' "$bad" >"$tmp/release/assets/install.sh"
    cp "$tmp/release/assets/install.sh" "$tmp/seed/install.sh"
    code=0
    run_sign "$devid" v9.9.9 --upload >"$tmp/t24.out" 2>&1 || code=$?
    expect 1 "$code" "an install.sh that is not the script is refused"
    expect_grep 'install.sh on it is not the installer' "$tmp/t24.out" "the refusal says so"
    expect_untouched true "an install.sh that is not the script"
done

# Each thing that makes it the script, wrong on its own in an otherwise real copy.
for wrong in first-line syntax team package; do
    scenario 9.9.9 v9.9.9 true
    echo always >"$tmp/spctl.mode"
    case $wrong in
        first-line) sed '1s/.*/#!\/bin\/bash/' "$install_sh" ;;
        syntax) cat "$install_sh"; printf 'if then\n' ;;
        team) sed "s/= $team_required \]/= ZZZZZZZZZZ ]/" "$install_sh" ;;
        package) sed 's/conbus-linux-aarch64.tar.gz/conbus-linux-other.tar.gz/' "$install_sh" ;;
    esac >"$tmp/release/assets/install.sh"
    cmp -s "$install_sh" "$tmp/release/assets/install.sh" && fail "$wrong: the copy did not change"
    cp "$tmp/release/assets/install.sh" "$tmp/seed/install.sh"
    code=0
    run_sign "$devid" v9.9.9 --upload >"$tmp/t24b.out" 2>&1 || code=$?
    expect 1 "$code" "an install.sh with the wrong $wrong is refused"
    expect_grep 'install.sh on it is not the installer' "$tmp/t24b.out" "the refusal says so"
    expect_untouched true "an install.sh with the wrong $wrong"
done

# --- after publishing, what people really get ---------------------------------

# GitHub's URLs can lag: a slow start is not an alarm.
scenario 9.9.9 v9.9.9 true
echo always >"$tmp/spctl.mode"
touch "$tmp/curl-slow-start"
code=0
run_sign "$devid" v9.9.9 --upload >"$tmp/t25.out" 2>&1 || code=$?
expect 0 "$code" "anonymous URLs that catch up are fine"
expect_nogrep 'IS PUBLISHED, AND' "$tmp/t25.out" "and raise no alarm"

for marker in curl-404 curl-stale latest-is-another; do
    scenario 9.9.9 v9.9.9 true
    echo always >"$tmp/spctl.mode"
    touch "$tmp/$marker"
    code=0
    run_sign "$devid" v9.9.9 --upload >"$tmp/t26.out" 2>&1 || code=$?
    expect 1 "$code" "$marker: a published release people cannot get fails the run"
    expect_grep 'IS PUBLISHED, AND' "$tmp/t26.out" "$marker: it says so loudly"
    case $marker in
        curl-404) said='cannot be fetched' ;;
        curl-stale) said='is not what was checked' ;;
        latest-is-another) said='as the latest, not v9.9.9' ;;
    esac
    expect_grep "$said" "$tmp/t26.out" "$marker: it says what is wrong"
    expect_grep 'gh release edit v9.9.9 --draft=true' "$tmp/t26.out" "$marker: it says how to take it back"
    expect_grep 'docs/release.md' "$tmp/t26.out" "$marker: it says where the rest is"
    expect false "$(cat "$tmp/release/draft")" "$marker: it does not undo anything by itself"
done

# --- packaging: no account's name in a public package ---------------------------

# package.sh as the workflow runs it, on this machine's own tar: BSD on the gate, GNU
# on Linux. rc.3's macOS package carried an operator's account name in its headers.
pack=$tmp/pack
mkdir -p "$pack/dist"
binary_of_version 9.9.9 >"$pack/conbus"
chmod 755 "$pack/conbus"
printf 'the licence\n' >"$pack/LICENSE"
printf 'the readme\n' >"$pack/README.md"
printf '## [9.9.9] - today\n\nnotes\n' >"$pack/CHANGELOG.md"
cp "$install_sh" "$pack/install.sh"
case $(uname -s) in
    Darwin)
        mkdir -p "$pack/target/aarch64-apple-darwin/release" "$pack/target/x86_64-apple-darwin/release"
        cp "$pack/conbus" "$pack/target/aarch64-apple-darwin/release/conbus"
        cp "$pack/conbus" "$pack/target/x86_64-apple-darwin/release/conbus"
        built=(conbus-macos-universal.tar.gz)
        args=(dist)
        ;;
    *)
        mkdir -p "$pack/target/x86_64-unknown-linux-musl/release"
        cp "$pack/conbus" "$pack/target/x86_64-unknown-linux-musl/release/conbus"
        built=(conbus-linux-x86_64.tar.gz)
        args=(dist x86_64-unknown-linux-musl)
        ;;
esac
: >"$tmp/codesign.calls"
(cd "$pack" && env PATH="$tools:$PATH" SR_TMP="$tmp" sh "$here/package.sh" "${args[@]}" >"$tmp/t27.out" 2>&1) ||
    { cat "$tmp/t27.out" >&2; fail "package.sh could not package a stand-in"; }
expect root/root "$(owners_of "$pack/dist/${built[0]}")" "package.sh names no account in its tar headers"
expect "conbus
LICENSE
THIRD_PARTY_NOTICES.txt
README.md
CHANGELOG.md" "$(tar -tzf "$pack/dist/${built[0]}")" "package.sh packs the same five members"
tar -xOzf "$pack/dist/${built[0]}" THIRD_PARTY_NOTICES.txt >"$tmp/notices.txt"
grep -q '^curve25519-dalek .* BSD-3-Clause$' "$tmp/notices.txt" ||
    fail "the package does not carry the notice of a crate built into the binary"
grep -q 'isis agora lovecruft' "$tmp/notices.txt" ||
    fail "the package names a crate and leaves out its copyright line"

# A binary without the stamp could not be checked at release time, so it stops here.
case $(uname -s) in
    Darwin)
        printf '#!/bin/sh\necho "conbus 9.9.9 (f4k3)"\n' >"$pack/target/aarch64-apple-darwin/release/conbus"
        ;;
    *)
        printf '#!/bin/sh\necho "conbus 9.9.9 (f4k3)"\n' >"$pack/target/x86_64-unknown-linux-musl/release/conbus"
        ;;
esac
code=0
(cd "$pack" && env PATH="$tools:$PATH" SR_TMP="$tmp" sh "$here/package.sh" "${args[@]}" >"$tmp/t28.out" 2>&1) || code=$?
expect 1 "$code" "package.sh refuses a binary without the version stamp"
expect_grep 'conbus-version 9.9.9;' "$tmp/t28.out" "it says what it looked for"

# --- the workflow -------------------------------------------------------------

# gh's --clobber deletes the asset it listed, and fails when that delete does. So
# two jobs replacing one asset fail each other, and one job owns each asset.
linux_upload=$(grep -c 'gh release upload "\$GITHUB_REF_NAME"' "$workflow" || true)
expect 1 "$linux_upload" "the workflow uploads to a release in one place"
expect_nogrep 'dist-linux/\*' "$workflow" "no Linux job uploads everything it built"
expect_grep 'dist-linux/conbus-linux-${{ matrix.asset_arch }}.tar.gz"$' "$workflow" \
    "each Linux job uploads its own package"
expect_grep 'dist-linux/conbus-linux-${{ matrix.asset_arch }}.tar.gz.sha256"$' "$workflow" \
    "each Linux job uploads its own checksum"
expect_grep 'gh release create "\$GITHUB_REF_NAME" dist/\* \\$' "$workflow" \
    "the release job attaches all it packaged"
expect_grep 'cp install.sh "\$out/install.sh"' "$here/package.sh" \
    "what it packaged includes install.sh"

echo "rehearsal: all checks passed"
