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
        .env("HOME", root)
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
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
    assert!(stdout.contains("Google sign-in"), "the page: {stdout}");
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

/// `this` as `conbus auth --url` answers it: the label alone.
fn page_label(root: &std::path::Path, url: &str) -> String {
    let output = conbus(root)
        .args(["auth", "--url", url])
        .output()
        .expect("run auth --url");
    assert!(
        output.status.success(),
        "{url}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    stdout
        .lines()
        .next()
        .and_then(|line| line.strip_prefix("this"))
        .unwrap_or_else(|| panic!("{url}: no `this` line in {stdout}"))
        .trim()
        .to_string()
}

/// Addresses that name a vendor anywhere but in their scheme and host. Each
/// one is a page the vendor does not serve.
const NOT_THE_VENDOR: &[&str] = &[
    "https://evil.example/phish?next=auth.openai.com",
    "https://auth.openai.com.evil.example/authorize",
    "https://evil.example/claude.ai/login",
    "https://notcursor.com/login",
    "https://netflix.ai/",
    "https://evil.example/?a=github.com/login",
    "https://evil.example/?x=chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/",
    "https://accounts.google.com.evil.example/o/oauth2/auth",
    "https://auth.openai.com@evil.example/",
    "https://auth.openai.com:443@evil.example/",
    "https://evil.example#auth.openai.com",
    "https://evil.example\\@auth.openai.com/",
    "https://auth.openai.com%2f.evil.example/",
    "https://evilx.ai/",
    "https://x.ai.evil.example/",
    "https://.x.ai/",
    "http://auth.openai.com/",
    "ftp://github.com/",
    "chrome-extension://aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa/connect.html",
    "chrome-extension://auth.openai.com/",
    "https://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html",
    "auth.openai.com/authorize",
    "not an address at all",
    "",
];

/// Addresses the vendors do serve, and the word each label must carry.
const THE_VENDOR: &[(&str, &str)] = &[
    (
        "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html",
        "Playwright MCP Bridge",
    ),
    (
        "https://accounts.google.com/o/oauth2/v2/auth?client_id=x",
        "Google",
    ),
    ("https://auth.openai.com/authorize", "OpenAI"),
    ("HTTPS://AUTH.OPENAI.COM/", "OpenAI"),
    ("https://claude.ai/login", "claude.ai"),
    ("https://console.anthropic.com/", "Anthropic Console"),
    ("https://cursor.com/loginDeepControl", "cursor.com"),
    ("https://www.cursor.com/", "cursor.com"),
    ("https://accounts.x.ai/sign-in", "x.ai"),
    ("https://x.ai/", "x.ai"),
    ("https://github.com/login/device", "github.com"),
    // The query names another vendor. The host is what counts.
    (
        "https://github.com/login/oauth/authorize?redirect_uri=https%3A%2F%2Fcursor.com%2Fcb",
        "github.com",
    ),
];

#[test]
fn a_page_is_named_by_its_scheme_and_host_and_nothing_else() {
    let root = tempfile::tempdir().expect("tempdir");
    for url in NOT_THE_VENDOR {
        assert_eq!(
            page_label(root.path(), url),
            "unknown page",
            "{url} is not the vendor's"
        );
    }
    for (url, word) in THE_VENDOR {
        let label = page_label(root.path(), url);
        assert!(label.contains(word), "{url} read as {label}");
    }
}

#[test]
fn a_broken_percent_sign_is_not_a_crash() {
    // The decoder used to slice a `str` at a byte offset, so a multibyte
    // character straight after `%` panicked with exit 101.
    let root = tempfile::tempdir().expect("tempdir");
    for url in [
        "https://x.example/?a=%a\u{e9}",
        "https://x.example/?token=SECRET-TOKEN-VALUE%a\u{e9}",
        "https://x.example/?client=SECRET-CLIENT%a\u{e9}",
        "https://x.example/?redirect_uri=%E0%A4%A",
        "https://x.example/?%a\u{e9}=1",
    ] {
        let output = conbus(root.path())
            .args(["auth", "--url", url])
            .output()
            .expect("run auth --url");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{url}: {stderr}");
        assert!(stdout.contains("unknown page"), "{url}: {stdout}");
        assert!(
            !stdout.contains("SECRET") && !stderr.contains("SECRET"),
            "{url} echoed a query value: {stdout} {stderr}"
        );
    }
}

/// node, or a failure that says why: a missing interpreter must not read as a
/// page script that behaves.
fn node() -> &'static str {
    match Command::new("node").arg("--version").output() {
        Ok(out) if out.status.success() => "node",
        Ok(out) => panic!("`node --version` exited {}", out.status),
        Err(err) => panic!(
            "node is not on PATH ({err}). These tests run the --html page's script in node \
             rather than skipping it"
        ),
    }
}

/// Run the page's own script against `urls`, as a browser would on a paste, and
/// return what it wrote for each. A script that throws fails the run.
fn page_script_says(page: &std::path::Path, urls: &[&str]) -> Vec<String> {
    const DRIVER: &str = r#"
const fs = require("fs");
const html = fs.readFileSync(process.argv[1], "utf8");
const script = html.match(/<script>([\s\S]*?)<\/script>/)[1];
const verdict = { textContent: "" };
const input = { value: "" };
globalThis.document = { getElementById: (id) => (id === "url" ? input : verdict) };
(0, eval)(script);
const said = JSON.parse(process.argv[2]).map((url) => {
  input.value = url;
  verdict.textContent = "(unset)";
  look();
  return verdict.textContent;
});
console.log(JSON.stringify(said));
"#;
    let urls_json = serde_json::to_string(urls).expect("json");
    let output = Command::new(node())
        .args(["-e", DRIVER])
        .arg(page)
        .arg(urls_json)
        .output()
        .expect("run node");
    assert!(
        output.status.success(),
        "the page's script threw: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("what the script wrote")
}

/// A page written with every vendor a stand-in, for the tests that only care
/// about its script.
fn a_page() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("tempdir");
    let page = root.path().join("auth.html");
    let output = conbus(root.path())
        .args(["auth", "--html", page.to_str().expect("utf8")])
        .output()
        .expect("run auth --html");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (root, page)
}

#[test]
fn the_pages_script_names_pages_the_way_the_command_does() {
    let (_root, page) = a_page();

    for (url, said) in NOT_THE_VENDOR
        .iter()
        .zip(page_script_says(&page, NOT_THE_VENDOR))
    {
        assert!(
            said.starts_with("this      unknown page"),
            "{url} is not the vendor's: {said}"
        );
    }
    let vendor_urls: Vec<&str> = THE_VENDOR.iter().map(|(url, _)| *url).collect();
    for ((url, word), said) in THE_VENDOR.iter().zip(page_script_says(&page, &vendor_urls)) {
        let first = said.lines().next().unwrap_or_default();
        assert!(first.contains(word), "{url} read as {first}");
    }
}

#[test]
fn a_bad_percent_in_the_pages_script_does_not_lose_the_verdict() {
    let (_root, page) = a_page();

    let said = page_script_says(
        &page,
        &["https://accounts.google.com/o/oauth2/auth?redirect_uri=%E0%A4%A&token=SECRET2"],
    );

    assert!(said[0].contains("Google"), "the verdict: {}", said[0]);
    assert!(!said[0].contains("SECRET2"), "{}", said[0]);
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
