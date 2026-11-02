//! `conbus quota` against a stand-in app-server: the real one needs an
//! account, and the shape it answers with was captured on 2026-09-03
//! (see `fixtures/codex_0.152.1_contract.md`).

use std::process::{Command, Stdio};
mod support;

/// A vendor binary that is not there. A test that does not name one runs
/// `conbus` against nothing on this machine: left unset, `quota` and `doctor`
/// ran the real `agy` and `codex app-server` on every `cargo test`.
const NOT_THERE: &str = "/nonexistent/conbus-test-vendor";

/// `conbus` with a data root of the test's own and no real vendor CLI. Each
/// test names the fakes it wants on top.
fn conbus(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_conbus"));
    command
        .env("CONBUS_HOME", root)
        .env("CONBUS_AGY_BIN", NOT_THERE)
        .env("CONBUS_CODEX_BIN", NOT_THERE);
    command
}

/// A stand-in `codex app-server`: speaks just enough JSON-RPC over stdio to
/// answer `initialize` and `account/rateLimits/read` with the captured shape.
const FAKE_APP_SERVER: &str = r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if msg.get("method") == "initialize":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"userAgent": "fake"}}), flush=True)
    elif msg.get("method") == "account/rateLimits/read":
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": {"rateLimits": {
            "limitId": "codex", "limitName": None,
            "primary": {"usedPercent": 22, "windowDurationMins": 300, "resetsAt": 4102444800},
            "secondary": {"usedPercent": 9, "windowDurationMins": 10080, "resetsAt": 4102444800},
            "credits": {"hasCredits": False, "unlimited": False, "balance": "0"},
            "individualLimit": None, "spendControlReached": False, "planType": "plus",
            "rateLimitReachedType": None},
            "rateLimitResetCredits": {"availableCount": 1, "credits": []},
            "accountId": "00000000-0000-0000-0000-000000000000", "rateLimitUpsell": None}}), flush=True)
"#;

fn fake_app_server() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("codex");
    support::write_executable(&path, FAKE_APP_SERVER);
    (dir, path)
}

/// Run a stand-in once, on its own time. On a loaded Mac the first exec of a newly
/// written script can take seconds, and `doctor` gives every probe two. That is a
/// fact about the machine and says nothing about the line under test.
fn first_exec_out_of_the_way(stand_in: &std::path::Path) {
    let status = Command::new(stand_in)
        .arg("app-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .status()
        .expect("run the stand-in");
    assert!(status.success(), "{status}");
}

#[test]
fn quota_reads_the_accounts_windows_through_the_app_server() {
    let (_dir, codex) = fake_app_server();

    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path())
        .arg("quota")
        .env("CONBUS_CODEX_BIN", &codex)
        .output()
        .expect("run quota");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("codex"), "{stdout}");
    assert!(stdout.contains("22%"), "the five-hour window: {stdout}");
    assert!(stdout.contains("9%"), "the weekly window: {stdout}");
    assert!(stdout.contains("plus"), "the plan: {stdout}");
    assert!(
        stdout.contains("not exhausted"),
        "the router's signal: {stdout}"
    );
}

#[test]
fn quota_says_so_when_the_app_server_is_not_there() {
    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path())
        .arg("quota")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .output()
        .expect("run quota");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "other legs still report when Codex is missing: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("codex") && stdout.contains("unavailable"),
        "the Codex line must name what could not be run: {stdout}"
    );
}

#[test]
fn doctor_carries_the_quota_line() {
    // The router's signal belongs where every other health fact is.
    let (_dir, codex) = fake_app_server();
    first_exec_out_of_the_way(&codex);
    let root = tempfile::tempdir().expect("tempdir");

    let output = conbus(root.path())
        .arg("doctor")
        .env("CONBUS_CODEX_BIN", &codex)
        .output()
        .expect("run doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.lines().any(|line| line.starts_with("quota ")
            && line.contains("22%")
            && line.contains("not exhausted")),
        "doctor must show the account's windows:\n{stdout}"
    );
}

#[test]
fn quota_prints_the_live_exhausted_signal() {
    // The 2026-09-06 capture, not the "primary" guess. A fake app-server that
    // speaks the live shape is how we know `conbus quota` will say so again.
    let reply = include_str!("fixtures/codex_0.152.1_quota_exhausted.json");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, sys
REPLY = json.loads({reply:?})
for line in sys.stdin:
    try:
        msg = json.loads(line)
    except Exception:
        continue
    if msg.get("method") == "initialize":
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": {{"userAgent": "fake"}}}}), flush=True)
    elif msg.get("method") == "account/rateLimits/read":
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": REPLY}}), flush=True)
"#
    );
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("codex");
    support::write_executable(&path, &script);

    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path())
        .arg("quota")
        .env("CONBUS_CODEX_BIN", &path)
        .output()
        .expect("run quota");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("EXHAUSTED (rate_limit_reached)"),
        "the live signal: {stdout}"
    );
    assert!(stdout.contains("100%"), "the five-hour window: {stdout}");
}

#[test]
fn quota_reads_agy_print_mode_json() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agy");
    let reply = include_str!("fixtures/agy_1.1.27_usage.json");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, sys
print(json.dumps(json.loads({reply:?})), flush=True)
"#
    );
    support::write_executable(&path, &script);

    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path())
        .arg("quota")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .env("CONBUS_AGY_BIN", &path)
        .output()
        .expect("run quota");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(stdout.contains("agy"), "{stdout}");
    assert!(stdout.contains("72%"), "the five-hour window: {stdout}");
    assert!(stdout.contains("31%"), "the weekly window: {stdout}");
    assert!(
        stdout.contains("not exhausted"),
        "the router's signal: {stdout}"
    );
}

#[test]
fn doctor_says_when_the_quota_cannot_be_read() {
    let root = tempfile::tempdir().expect("tempdir");

    let output = conbus(root.path())
        .arg("doctor")
        .env("CONBUS_CODEX_BIN", "/nonexistent/codex")
        .output()
        .expect("run doctor");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout
            .lines()
            .any(|line| line.starts_with("quota ") && line.contains("unavailable")),
        "an unreadable quota must be said, not omitted:\n{stdout}"
    );
}

/// A stand-in `agy` that behaves as 1.4.12 was measured to in a throwaway HOME,
/// and the marks it leaves. `models` is the given shell; `-p`, with no login,
/// opens the browser through `open` on PATH, the way the real one does.
struct AgyStandIn {
    dir: tempfile::TempDir,
}

impl AgyStandIn {
    fn new(models: &str, print: &str) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let calls = dir.path().join("calls");
        let opened = dir.path().join("opened");
        support::write_executable(
            &dir.path().join("agy"),
            &format!(
                "#!/bin/sh\necho \"$*\" >>'{}'\ncase \"$1\" in\n  models) {models} ;;\n  -p) {print} ;;\nesac\n",
                calls.display()
            ),
        );
        support::write_executable(
            &dir.path().join("open"),
            &format!("#!/bin/sh\necho \"$*\" >>'{}'\n", opened.display()),
        );
        Self { dir }
    }

    /// With no login: `models` says so, `-p` goes for the browser.
    fn not_signed_in() -> Self {
        Self::new(
            "echo 'Fetching available models...' >&2; echo 'Error: Please sign in to view available models. Launch the CLI without arguments to sign in.' >&2; exit 1",
            "open https://accounts.example/o/oauth2/auth; echo 'Authentication required.' >&2; exit 1",
        )
    }

    /// `conbus` with this `agy` and this `open` found first on PATH.
    fn run(&self, root: &std::path::Path, sub: &str) -> String {
        let path = format!(
            "{}:{}",
            self.dir.path().display(),
            std::env::var("PATH").expect("PATH")
        );
        let output = conbus(root)
            .arg(sub)
            .env("CONBUS_AGY_BIN", self.dir.path().join("agy"))
            .env("CONBUS_SKIP_LAUNCHCTL", "1")
            .env("PATH", path)
            .output()
            .expect("run conbus");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }

    fn opened_a_browser(&self) -> bool {
        self.dir.path().join("opened").exists()
    }

    /// `models` at most: a deadline can kill the stand-in before it writes.
    fn assert_only_the_check_ran(&self, what: &str) {
        let calls = self.calls();
        assert!(
            calls.iter().all(|call| call == "models"),
            "{what} went past the check: {calls:?}"
        );
    }
}

fn agy_line(stdout: &str) -> &str {
    stdout
        .lines()
        .find(|line| line.starts_with("agy ") || line.contains(" agy "))
        .unwrap_or_else(|| panic!("no agy line in:\n{stdout}"))
}

/// The one thing nobody asked for: a browser tab per probe, from `doctor`, a
/// canary, or a hop the daemon started, on an account that is not signed in.
#[test]
fn an_agy_with_no_login_is_never_asked_for_usage() {
    for sub in ["quota", "doctor", "status"] {
        let agy = AgyStandIn::not_signed_in();
        let root = tempfile::tempdir().expect("tempdir");

        let stdout = agy.run(root.path(), sub);

        let line = agy_line(&stdout);
        assert!(
            line.contains("AUTH_REQUIRED")
                // `doctor` allows two seconds, which a busy machine can use up
                // before the stand-in has run. Not knowing is still not asking.
                || (sub == "doctor" && line.contains("did not answer within 2s")),
            "`{sub}` says so:\n{stdout}"
        );
        assert!(!agy.opened_a_browser(), "`{sub}` opened a browser");
        agy.assert_only_the_check_ran(sub);
    }
}

/// A vendor that exits clean while asking for a login must not open the gate.
#[test]
fn a_request_to_sign_in_outranks_a_clean_exit() {
    let agy = AgyStandIn::new(
        "echo 'Please sign in to view available models.'; exit 0",
        "open https://accounts.example/o/oauth2/auth; exit 1",
    );
    let root = tempfile::tempdir().expect("tempdir");

    let stdout = agy.run(root.path(), "quota");

    assert!(agy_line(&stdout).contains("AUTH_REQUIRED"), "{stdout}");
    assert!(!agy.opened_a_browser(), "a clean exit opened a browser");
    assert_eq!(agy.calls(), ["models"], "a clean exit went past the check");
}

/// `hop` has no line to say it on, and the daemon starts hops with nobody
/// watching: it goes on without a reading and asks for none.
#[test]
fn a_hop_from_an_agy_with_no_login_asks_for_no_usage() {
    let agy = AgyStandIn::not_signed_in();
    let root = tempfile::tempdir().expect("tempdir");
    let log = root.path().join("log");
    std::fs::create_dir_all(&log).expect("log dir");
    let segment: String = include_str!("../../conbus-core/tests/fixtures/segment-v3.jsonl")
        .lines()
        .take(2)
        .map(|line| line.replace("\"claude_code\"", "\"agy\"") + "\n")
        .collect();
    std::fs::write(log.join("segment-00000000.jsonl"), segment).expect("segment");

    let stdout = agy.run(root.path(), "hop");

    assert!(
        !agy.calls().is_empty(),
        "the hop never reached agy:\n{stdout}"
    );
    agy.assert_only_the_check_ran("a hop");
    assert!(!agy.opened_a_browser(), "a hop opened a browser");
}

#[test]
fn a_signed_in_agy_is_read_once_the_check_says_so() {
    let dir = tempfile::tempdir().expect("tempdir");
    let reply = dir.path().join("reply.json");
    std::fs::write(&reply, include_str!("fixtures/agy_1.1.27_usage.json")).expect("reply");
    let agy = AgyStandIn::new(
        "printf 'gemini-pro\\tGemini Pro\\n'",
        &format!("cat '{}'", reply.display()),
    );
    let root = tempfile::tempdir().expect("tempdir");

    let stdout = agy.run(root.path(), "quota");

    assert!(agy_line(&stdout).contains("72%"), "the reading:\n{stdout}");
    assert!(!stdout.contains("AUTH_REQUIRED"), "{stdout}");
    assert!(!stdout.contains("SKIP"), "{stdout}");
    let calls = agy.calls();
    assert_eq!(calls.len(), 2, "{calls:?}");
    assert_eq!(calls[0], "models");
    assert!(calls[1].starts_with("-p /usage"), "{calls:?}");
}

/// Failing closed: only a clean answer lets `-p` run. A check that fails for a
/// reason the build does not recognise says nothing about the login.
#[test]
fn an_agy_whose_login_cannot_be_told_is_skipped_not_asked() {
    for (models, why) in [
        ("echo 'Error: network unreachable' >&2; exit 3", "exit 3"),
        ("exit 0", "an empty list"),
    ] {
        let agy = AgyStandIn::new(
            models,
            "open https://accounts.example/o/oauth2/auth; exit 1",
        );
        let root = tempfile::tempdir().expect("tempdir");

        let stdout = agy.run(root.path(), "quota");

        assert!(
            agy_line(&stdout).contains("SKIP"),
            "{why} says it was skipped:\n{stdout}"
        );
        assert!(
            !agy_line(&stdout).contains("AUTH_REQUIRED"),
            "{why} is not a known lack of login:\n{stdout}"
        );
        assert!(!agy.opened_a_browser(), "{why} opened a browser");
        assert_eq!(agy.calls(), ["models"], "{why} went past the check");
    }
}

#[test]
fn a_login_check_that_hangs_is_skipped_within_doctors_deadline() {
    let agy = AgyStandIn::new(
        "exec sleep 60",
        "open https://accounts.example/o/oauth2/auth; exit 1",
    );
    let root = tempfile::tempdir().expect("tempdir");

    let started = std::time::Instant::now();
    let stdout = agy.run(root.path(), "doctor");

    assert!(
        started.elapsed() < std::time::Duration::from_secs(20),
        "doctor held for {:?}",
        started.elapsed()
    );
    let line = agy_line(&stdout);
    assert!(line.contains("SKIP"), "{stdout}");
    assert!(line.contains("did not answer within 2s"), "{stdout}");
    assert!(!agy.opened_a_browser(), "a hung check opened a browser");
    agy.assert_only_the_check_ran("a hung check");
}
