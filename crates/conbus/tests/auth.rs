//! `conbus auth`: the vendor table with stand-ins, `--url` naming a page
//! without ever echoing its query values, and `--html` writing one
//! self-contained page that reaches no network.

use std::process::Command;
mod support;

const NOT_THERE: &str = "/nonexistent/conbus-test-vendor";

fn conbus(root: &std::path::Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_conbus"));
    command
        .env("CONBUS_HOME", root)
        .env("CONBUS_AGY_BIN", NOT_THERE)
        .env("CONBUS_CODEX_BIN", NOT_THERE)
        .env("CONBUS_GH", NOT_THERE);
    command
}

/// An `agy models` that reports no login; `-p` writes a marker to `calls`
/// the way the real one would reach OAuth — it must never run.
fn fake_logged_out_agy() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let calls = dir.path().join("calls");
    let script = format!(
        "#!/bin/sh\nprintf '%s\\n' \"$1\" >>\"{}\"\n\
         if [ \"$1\" = models ]; then\n  echo 'sign in required' >&2\n  exit 1\nfi\n",
        calls.display()
    );
    let path = dir.path().join("agy");
    support::write_executable(&path, &script);
    (dir, path, calls)
}

/// A `gh` that answers `auth status` like a signed-in one.
fn fake_gh(exit_code: i32) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gh");
    support::write_executable(
        &path,
        &format!(
            "#!/bin/sh\nif [ \"$1 $2\" = 'auth status' ]; then exit {exit_code}; fi\nexit 1\n"
        ),
    );
    (dir, path)
}

#[test]
fn auth_table_names_what_needs_a_login_and_exit_stays_zero() {
    let (_agy_dir, agy, calls) = fake_logged_out_agy();
    let (_gh_dir, gh) = fake_gh(0);
    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path())
        .arg("auth")
        .env("CONBUS_AGY_BIN", &agy)
        .env("CONBUS_GH", &gh)
        .output()
        .expect("run auth");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "a vendor without login must not fail the command: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("needs no login of its own"),
        "the conbus line: {stdout}"
    );
    assert!(
        stdout.contains("nothing conbus runs on its own"),
        "the background line: {stdout}"
    );
    assert!(stdout.contains("AUTH_REQUIRED"), "agy's line: {stdout}");
    assert!(
        stdout.contains("gh") && stdout.contains("signed in"),
        "gh's line: {stdout}"
    );
    for hook_only in ["cursor", "grok", "opencode"] {
        assert!(
            stdout.contains(hook_only) && stdout.contains("not used"),
            "{hook_only} is hooks only: {stdout}"
        );
    }
    // `auth` may check the login question but never `-p`, which is what
    // opened the browser tabs that made this command necessary.
    let called = std::fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        !called.lines().any(|line| line == "-p"),
        "auth must never run `agy -p`: {called}"
    );
}

#[test]
fn auth_url_names_the_page_without_echoing_its_query() {
    let root = tempfile::tempdir().expect("tempdir");
    let url = "https://accounts.google.com/o/oauth2/v2/auth?client_id=SECRETID&scope=EMAILSCOPE&redirect_uri=http%3A%2F%2F127.0.0.1%3A59999%2Fcb";
    let output = conbus(root.path())
        .args(["auth", "--url", url])
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(stdout.contains("Google OAuth"), "the pattern: {stdout}");
    for secret in ["SECRETID", "EMAILSCOPE"] {
        assert!(
            !stdout.contains(secret),
            "query values must never print: {secret} in {stdout}"
        );
    }
    assert!(stdout.contains("conbus"), "the conbus verdict: {stdout}");
}

#[test]
fn auth_url_finds_the_token_file_and_the_port_holder() {
    // The Playwright bridge URL is the one that was actually chased down:
    // its token lives in a vendor config and its port names the asker.
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path();
    let codex = home.join(".codex");
    std::fs::create_dir_all(&codex).expect("mkdir");
    std::fs::write(
        codex.join("config.toml"),
        "mcp_servers.playwright.token = \"TOK3N-USED-FOR-TEST\"\n",
    )
    .expect("config");

    // A port the test itself holds: ancestry must reach this process.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let url = format!(
        "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html?mcpRelayUrl=ws%3A%2F%2F127.0.0.1%3A{port}%2F&token=TOK3N-USED-FOR-TEST&client=%7B%22name%22%3A%22test-client%22%7D"
    );
    let output = conbus(dir.path())
        .args(["auth", "--url", &url])
        .env("HOME", home)
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        stdout.contains("Playwright MCP Bridge"),
        "the page pattern: {stdout}"
    );
    assert!(
        stdout.contains("test-client"),
        "the client's name is the one query value safe to show: {stdout}"
    );
    assert!(
        !stdout.contains("TOK3N-USED-FOR-TEST"),
        "the token value must never print: {stdout}"
    );
    assert!(
        stdout.contains(".codex/config.toml") || stdout.contains("came from"),
        "which file held the token: {stdout}"
    );
    if which("lsof") {
        assert!(
            stdout.contains("cargo") || stdout.contains("conbus auth") || stdout.contains(" > "),
            "the holder's ancestry: {stdout}"
        );
    }
}

fn which(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[test]
fn auth_unknown_url_says_so() {
    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path())
        .args(["auth", "--url", "https://example.net/?q=1"])
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("unknown page"), "{stdout}");
}

/// A stand-in `codex app-server`: just enough JSON-RPC to answer
/// `account/rateLimits/read` like a signed-in account.
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

#[test]
fn auth_html_is_self_contained_and_owner_only() {
    let agy_dir = tempfile::tempdir().expect("tempdir");
    let agy = agy_dir.path().join("agy");
    support::write_executable(
        &agy,
        "#!/bin/sh\nif [ \"$1\" = models ]; then\n  echo 'claude-opus'\nfi\n",
    );
    let codex_dir = tempfile::tempdir().expect("tempdir");
    let codex = codex_dir.path().join("codex");
    support::write_executable(&codex, FAKE_APP_SERVER);
    let (_gh_dir, gh) = fake_gh(0);

    let root = tempfile::tempdir().expect("tempdir");
    let page = root.path().join("auth.html");
    let output = conbus(root.path())
        .args(["auth", "--html", page.to_str().expect("utf8")])
        .env("CONBUS_AGY_BIN", &agy)
        .env("CONBUS_CODEX_BIN", &codex)
        .env("CONBUS_GH", &gh)
        .output()
        .expect("run auth --html");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let html = std::fs::read_to_string(&page).expect("read the page");
    assert!(
        html.contains("default-src 'none'"),
        "the CSP must fence everything off: {html}"
    );
    assert!(
        !html.contains("src=\"http") && !html.contains("href=\"http"),
        "no external resource may be referenced"
    );
    for phrase in [
        "conbus needs no login",
        "conbus에 지금 필요한 로그인은 없습니다",
        "conbus는 스스로 로그인 페이지를 열지 않습니다",
    ] {
        assert!(html.contains(phrase), "banner must carry: {phrase}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&page).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the page is owner-only: {mode:o}");
    }
}
