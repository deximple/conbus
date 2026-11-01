#!/bin/bash
# A vendor CLI with no login, driven by conbus, must not open a browser.
#
#   scripts/check-no-browser.sh <conbus binary>
#
# Runs doctor, quota and status with the real `agy` and `codex` in a throwaway
# HOME and an empty environment, where neither is signed in. `open` on PATH is a
# stand-in that writes a mark. A sandbox denies the real `open`, `osascript` and
# Launch Services, so a failure opens no tab by those routes.
#
#   exit 0  no browser, and agy's line says AUTH_REQUIRED
#   exit 1  a browser would have opened, or the line is wrong
#   exit 2  nothing proved: no agy, no sandbox, or the control below saw nothing
#
# A control runs `agy -p /usage` first. It has to go for the browser, or this
# check can no longer see one and a pass would mean nothing. tests/quota.rs
# holds conbus's half in CI, which has no agy; this holds the vendor's half, so
# run it after an agy upgrade.
#
# It reads nothing from the real HOME and needs no credentials.
set -u

conbus=${1:-}
if [ -z "$conbus" ] || [ ! -x "$conbus" ]; then
    echo "usage: $0 <conbus binary>" >&2
    exit 2
fi
case $conbus in
    /*) ;;
    *) conbus=$PWD/$conbus ;;
esac
if ! command -v agy >/dev/null 2>&1; then
    echo "no-browser: nothing proved: no agy on PATH" >&2
    exit 2
fi
if [ ! -x /usr/bin/sandbox-exec ]; then
    echo "no-browser: nothing proved: needs sandbox-exec, so that a failure cannot open a tab" >&2
    exit 2
fi

tmp=$(mktemp -d /tmp/cb-no-browser.XXXXXX)
control=
trap '[ -z "$control" ] || kill -TERM "$control" 2>/dev/null; rm -rf "$tmp"' EXIT
mkdir -p "$tmp/bin"
cat >"$tmp/block.sb" <<'EOF'
(version 1)
(allow default)
(deny process-exec (literal "/usr/bin/open") (literal "/usr/bin/osascript"))
(deny mach-lookup
    (global-name "com.apple.coreservices.launchservicesd")
    (global-name "com.apple.lsd.mapdb")
    (global-name "com.apple.lsd.modifydb"))
EOF
cat >"$tmp/bin/open" <<EOF
#!/bin/sh
echo "\$*" >>"\$HOME/open.called"
EOF
chmod 755 "$tmp/bin/open"

# Become a command run as the throwaway user $1, with the sandbox in front of
# it. Call in a subshell: the exec keeps $! on the process itself, so a kill
# reaches it. A control left behind waits on its OAuth flow.
as_nobody() {
    home=$tmp/$1
    shift
    mkdir -p "$home"
    exec env -i HOME="$home" CONBUS_HOME="$home/.conbus" CONBUS_SKIP_LAUNCHCTL=1 \
        PATH="$tmp/bin:$PATH" TERM=dumb USER="${USER:-}" \
        /usr/bin/sandbox-exec -f "$tmp/block.sb" "$@"
}

# How many OAuth flows agy's own logs recorded under throwaway user $1.
oauth_starts() {
    grep -rho 'Starting OAuth' "$tmp/$1/.gemini" 2>/dev/null | wc -l | tr -d ' '
}

went_for_browser() {
    [ -e "$tmp/$1/open.called" ] || [ "$(oauth_starts "$1")" != 0 ]
}

# --- control: the detector still detects -------------------------------------

(as_nobody control agy -p /usage --output-format json --print-timeout 3s) \
    >"$tmp/control.out" 2>&1 </dev/null &
control=$!
for _ in $(seq 1 60); do
    [ -e "$tmp/control/open.called" ] && break
    sleep 0.5
done
kill -TERM "$control" 2>/dev/null
wait "$control" 2>/dev/null
control=
if ! went_for_browser control; then
    echo "no-browser: nothing proved: \`agy -p\` with no login showed no browser and no OAuth," >&2
    echo "no-browser: so this check can no longer see one" >&2
    sed 's/^/  control: /' "$tmp/control.out" >&2
    exit 2
fi
echo "no-browser: control: agy -p went for the browser (open called: $([ -e "$tmp/control/open.called" ] && echo yes || echo no), OAuth starts: $(oauth_starts control))"

# --- the check ---------------------------------------------------------------

failed=0
check() { # check <label> <grep -E pattern for agy's line> <command...>
    label=$1
    pattern=$2
    shift 2
    user=user-$label
    (as_nobody "$user" "$@") >"$tmp/$user.out" 2>&1 </dev/null
    line=$(grep -E '^agy |^quota +agy ' "$tmp/$user.out" | head -n 1)
    if went_for_browser "$user"; then
        echo "no-browser: FAIL: $label went for the browser (OAuth starts: $(oauth_starts "$user")) — $line"
        failed=1
    elif ! printf '%s\n' "$line" | grep -Eq "$pattern"; then
        echo "no-browser: FAIL: $label: agy's line does not say $pattern: $line"
        failed=1
    else
        echo "no-browser: ok: $label — $line"
        grep -E '^codex |^quota +codex ' "$tmp/$user.out" | head -n 1 | sed 's/^/no-browser:   and /'
    fi
}

# doctor allows two seconds, which a busy machine can use up: not knowing is
# still not asking.
doctor_says='AUTH_REQUIRED|SKIP: .*did not answer within 2s'
check doctor "$doctor_says" "$conbus" doctor
check quota AUTH_REQUIRED "$conbus" quota
check status AUTH_REQUIRED "$conbus" status
# The same with a terminal, where `agy -p` might behave differently.
check doctor-tty "$doctor_says" /usr/bin/script -q /dev/null "$conbus" doctor

if [ "$failed" != 0 ]; then
    exit 1
fi
echo "no-browser: pass"
