#!/bin/sh
# Sign a published release with a Developer ID, have Apple notarize it, and
# replace the ad-hoc package on the GitHub release with the signed one. A stable
# tag's release is a draft until this has run with --upload, which refuses to
# publish it unless everything an install fetches is on it.
#
#   CONBUS_SIGN_IDENTITY="Developer ID Application: Name (TEAMID)" \
#     scripts/sign-release.sh v0.2.6            # sign and notarize into target/signed/
#   CONBUS_SIGN_IDENTITY=… scripts/sign-release.sh v0.2.6 --upload
#   CONBUS_SIGN_IDENTITY=- scripts/sign-release.sh v0.2.6   # rehearsal: ad hoc, no Apple
#
# What the operator sets up once, because each step needs their sign-in:
#   1. A Developer ID Application certificate in the login keychain (Xcode →
#      Settings → Accounts → the paid team → Manage Certificates → +).
#   2. Notary credentials in the keychain, stored under a profile name:
#        xcrun notarytool store-credentials conbus-notary \
#          --apple-id APPLE_ID --team-id TEAMID
#      (it asks for an app-specific password from appleid.apple.com).
# This script never sees a password: codesign uses the keychain's key, and
# notarytool the stored profile (CONBUS_NOTARY_PROFILE, default conbus-notary).
#
# The package keeps its name and its members, so install.sh and
# releases/latest/download/<name> work unchanged. A bare Mach-O cannot carry
# a stapled ticket; Gatekeeper looks the notarization up online the first time
# it checks the binary.
set -eu

die() { echo "sign-release.sh: $*" >&2; exit 1; }

tag=${1:-}
upload=${2:-}
case $tag in
  "" | *[!v0-9.rc-]*) die "usage: scripts/sign-release.sh vX.Y.Z[-rc.N] [--upload]" ;;
esac
printf '%s\n' "$tag" | grep -Eqx 'v[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?' ||
  die "usage: scripts/sign-release.sh vX.Y.Z[-rc.N] [--upload]"

case $upload in
  ""|--upload) ;;
  *) die "unknown argument $upload; the only option is --upload" ;;
esac
identity=${CONBUS_SIGN_IDENTITY:-}
[ -n "$identity" ] || die 'set CONBUS_SIGN_IDENTITY to "Developer ID Application: … (TEAMID)", or to - for a rehearsal'
profile=${CONBUS_NOTARY_PROFILE:-conbus-notary}
rehearsal=false
[ "$identity" = "-" ] && rehearsal=true
if $rehearsal && [ -n "$upload" ]; then
  die "a rehearsal signs ad hoc and is never uploaded"
fi

asset=conbus-macos-universal.tar.gz
out=target/signed
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT INT TERM

# What an install fetches: the package install.sh names for each OS and
# architecture, the checksum beside each, and install.sh, which the release job
# attaches. A release that lacks one of these 404s for somebody.
required="$asset $asset.sha256
conbus-linux-x86_64.tar.gz conbus-linux-x86_64.tar.gz.sha256
conbus-linux-aarch64.tar.gz conbus-linux-aarch64.tar.gz.sha256
install.sh"

case $tag in
  *-rc.*) stable=false held="a prerelease" ;;
  *) stable=true held="a draft" ;;
esac

# Nothing has been published. Says what is wrong and what to do about it.
refuse() {
  die "$tag stays $held: $1
  The release job attaches the macOS package, its checksum and install.sh; the
  two release-linux jobs attach the Linux packages and theirs. To see them:
    gh run list --workflow release.yml --branch $tag
  To run a failed job again:
    gh run rerun RUN_ID --failed
  If an upload from here stopped part way, $out holds the signed package:
    gh release upload $tag $out/$asset $out/$asset.sha256 --clobber
  Then run this again with --upload."
}

# Fetch everything an install fetches, as an install fetches it, and check each
# checksum. gh fails only when no pattern matches, and reports its own error, so
# what arrived is what counts.
check_release() {
  rm -rf "$work/check"
  mkdir "$work/check"
  patterns=
  for name in $required; do
    patterns="$patterns -p $name"
  done
  # shellcheck disable=SC2086 # one word per flag and per name
  gh release download "$tag" $patterns -D "$work/check" || true
  missing=
  for name in $required; do
    [ -s "$work/check/$name" ] || missing="$missing $name"
  done
  [ -z "$missing" ] || refuse "it is missing$missing"
  for name in $required; do
    case $name in
      *.sha256)
        (cd "$work/check" && shasum -a 256 -c "$name" >/dev/null 2>&1) ||
          refuse "${name%.sha256} does not match the checksum beside it"
        ;;
    esac
  done
}

# Before any signing: Apple's first review of a release has taken hours, which is
# a long wait to learn that the Linux jobs have not finished. A stable release
# already public is not this script's to change: it would replace a live package
# and make an old tag `latest` again.
if [ -n "$upload" ]; then
  if $stable; then
    draft=$(gh release view "$tag" --json isDraft --jq .isDraft) ||
      die "could not read the release $tag"
    [ "$draft" = true ] ||
      die "$tag is already published; this only finishes a draft, and changed nothing"
  fi
  check_release
fi

# 1. The package as published, checked against its own checksum.
gh release download "$tag" -p "$asset" -p "$asset.sha256" -D "$work"
(cd "$work" && shasum -a 256 -c "$asset.sha256" >/dev/null) ||
  die "$asset does not match the $asset.sha256 published beside it"
members=$(tar -tzf "$work/$asset")
mkdir "$work/stage"
tar -xzf "$work/$asset" -C "$work/stage"
bin="$work/stage/conbus"
version=$("$bin" --version | awk '{print $2}')
[ "v$version" = "$tag" ] || die "the package is conbus $version, not $tag"

# 2. Both slices: the hardened runtime notarization requires, a secure
# timestamp, the identifier the ad-hoc packages carry, and one entitlement
# (conbus.entitlements says why).
entitlements=$(dirname "$0")/conbus.entitlements
[ -f "$entitlements" ] || die "missing $entitlements"
if $rehearsal; then
  codesign --force --options runtime --entitlements "$entitlements" \
    --identifier sh.conbus --sign - "$bin"
else
  codesign --force --options runtime --timestamp --entitlements "$entitlements" \
    --identifier sh.conbus --sign "$identity" "$bin"
fi
codesign --verify --strict "$bin" || die "the signed binary does not verify"
for arch in arm64 x86_64; do
  codesign -dv --arch "$arch" "$bin" 2>&1 | grep -q 'runtime' ||
    die "the $arch slice is not signed with the hardened runtime"
done
if ! $rehearsal; then
  team=$(codesign -dv "$bin" 2>&1 | sed -n 's/^TeamIdentifier=//p')
  [ -n "$team" ] && [ "$team" != "not set" ] ||
    die "the signature names no team: $identity is not a Developer ID"
fi
"$bin" --version >/dev/null || die "the signed binary does not run"

# 3. Notarization: Apple checks a zip of the binary and keeps the ticket,
# keyed to the code's hash. Signing the same binary the same way again gives
# the same hash, so a run after a failure finds the ticket and does not wait
# on Apple twice.
#
# Gatekeeper's `--type execute` assesses apps only, and says a command-line
# binary "does not seem to be an app" whether or not it is notarized. This is
# the assessment for a file that is not an app.
notarized() {
  spctl --assess --type open --context context:primary-signature --verbose "$1" 2>&1 |
    grep -q 'source=Notarized Developer ID'
}
if ! $rehearsal; then
  if notarized "$bin"; then
    echo "sign-release.sh: Apple has already notarized this code; not submitting again"
  else
    ditto -c -k --keepParent "$bin" "$work/conbus.zip"
    xcrun notarytool submit "$work/conbus.zip" --keychain-profile "$profile" \
      --wait --output-format json > "$work/notary.json" ||
      die "notarytool could not submit; is the $profile profile stored?"
    grep -q '"status" *: *"Accepted"' "$work/notary.json" || {
      cat "$work/notary.json" >&2
      id=$(sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' "$work/notary.json" | head -1)
      [ -n "$id" ] && xcrun notarytool log "$id" --keychain-profile "$profile" >&2
      die "Apple did not accept $tag for notarization"
    }
  fi
  # Apple publishes a ticket a few minutes after accepting it: v0.2.7 was
  # accepted at 09:02Z on 2026-09-28 and Gatekeeper first saw it at 09:04:42Z.
  # A check straight after acceptance fails without this wait.
  tries=0
  until notarized "$bin"; do
    tries=$((tries + 1))
    [ "$tries" -lt 40 ] ||
      die "Gatekeeper still does not report the binary as notarized after 10 minutes; run again later, and the ticket Apple issued will be found"
    [ "$tries" -eq 1 ] && echo "sign-release.sh: waiting for Apple to publish the ticket"
    sleep 15
  done
  codesign --verify --test-requirement="=notarized" "$bin" ||
    die "the binary does not satisfy the notarized requirement"
fi

# 4. The same package, members in the same order, with the signed binary.
rm -rf "$out"
mkdir -p "$out"
# shellcheck disable=SC2086 # the member names are the package's own
tar -czf "$out/$asset" -C "$work/stage" $members
(cd "$out" && shasum -a 256 "$asset" > "$asset.sha256")
echo "sign-release.sh: $out/$asset ($(cut -d' ' -f1 < "$out/$asset.sha256"))"

if [ -n "$upload" ]; then
  gh release upload "$tag" "$out/$asset" "$out/$asset.sha256" --clobber
  # Read it back. A job still running, or run again, can have changed the release
  # since the first look, and this is the last look before people get it.
  check_release
  cmp -s "$work/check/$asset" "$out/$asset" ||
    refuse "the $asset on it is not the signed one in $out"
  echo "sign-release.sh: $tag now carries the signed, notarized package and all an install needs"
  # A release tag was created as a draft so that `latest` never pointed at the
  # ad-hoc package; only now, signed, does it become the release people get.
  # A release candidate stays a prerelease and never becomes `latest`.
  if $stable; then
    gh release edit "$tag" --draft=false --latest
    echo "sign-release.sh: $tag is published and is now the latest release"
  fi
elif ! $rehearsal; then
  echo "sign-release.sh: check it, then run again with --upload"
fi
