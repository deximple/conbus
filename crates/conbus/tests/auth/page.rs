//! `--html`: the page, its banner, and when it is opened.

use super::*;

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

/// A stand-in `codex app-server` whose answer to the limits read is an error
/// the vendor wrote.
fn codex_refusing_with(message: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("codex");
    let message = serde_json::to_string(message).expect("json");
    let script = format!(
        r#"#!/usr/bin/env python3
import json, sys
for line in sys.stdin:
    msg = json.loads(line)
    if msg.get("method") == "initialize":
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "result": {{"userAgent": "fake"}}}}), flush=True)
    elif "id" in msg:
        print(json.dumps({{"jsonrpc": "2.0", "id": msg["id"], "error": {{"code": -1, "message": {message}}}}}), flush=True)
"#
    );
    support::write_executable(&path, &script);
    (dir, path)
}

/// A stand-in `codex app-server` that reads as signed in.
fn codex_signed_in() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("codex");
    support::write_executable(&path, FAKE_APP_SERVER);
    (dir, path)
}

/// An `agy models` that lists a model.
fn agy_signed_in() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agy");
    support::write_executable(
        &path,
        "#!/bin/sh\nif [ \"$1\" = models ]; then\n  echo 'some-model'\nfi\n",
    );
    warmed(&path);
    (dir, path)
}

/// The page, written with these vendors in place of the real ones.
fn page_with(vendors: &[(&str, &std::path::Path)]) -> String {
    let root = tempfile::tempdir().expect("tempdir");
    let page = root.path().join("auth.html");
    let mut command = conbus(root.path());
    command.args(["auth", "--html", page.to_str().expect("utf8")]);
    for (name, path) in vendors {
        command.env(name, path);
    }
    let output = command.output().expect("run auth --html");
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    std::fs::read_to_string(&page).expect("read the page")
}

/// Which banner a page opens with.
fn banner(html: &str) -> &str {
    let (_, rest) = html
        .split_once("<div class=\"banner ")
        .unwrap_or_else(|| panic!("no banner: {html}"));
    rest.split('"').next().expect("class")
}

#[test]
fn a_script_in_a_vendors_error_stays_text() {
    let (_codex_dir, codex) = codex_refusing_with(
        "<script>location.href=String.fromCharCode(47)+document.title</script><img src=x onerror=alert(1)>\"'&",
    );

    let html = page_with(&[("CONBUS_CODEX_BIN", &codex)]);

    assert_eq!(
        html.matches("<script").count(),
        1,
        "only the page's own script: {html}"
    );
    assert!(!html.contains("<img"), "{html}");
    assert!(
        html.contains("&lt;script&gt;location.href") && html.contains("&lt;img src=x"),
        "the text is still there to read: {html}"
    );
    assert!(html.contains("&amp;"), "{html}");
    assert!(html.contains("&#39;") && html.contains("&quot;"), "{html}");
}

#[test]
fn control_characters_in_a_vendors_error_reach_neither_page_nor_terminal() {
    let (_codex_dir, codex) = codex_refusing_with("bad\u{1b}[2K\nconbus    no: forged");
    let root = tempfile::tempdir().expect("tempdir");

    let output = conbus(root.path())
        .arg("auth")
        .env("CONBUS_CODEX_BIN", &codex)
        .output()
        .expect("run auth");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
    assert!(
        !stdout.lines().any(|l| l.starts_with("conbus    no")),
        "{stdout}"
    );
    let html = page_with(&[("CONBUS_CODEX_BIN", &codex)]);
    assert!(!html.contains('\u{1b}'), "{html:?}");
}

#[test]
fn the_banner_is_green_only_with_nothing_signed_out() {
    let (_codex_dir, codex) = codex_signed_in();
    let (_agy_dir, agy) = agy_signed_in();
    let (_gh_dir, gh) = fake_gh(0);

    let all_in = page_with(&[
        ("CONBUS_CODEX_BIN", &codex),
        ("CONBUS_AGY_BIN", &agy),
        ("CONBUS_GH", &gh),
    ]);
    assert_eq!(banner(&all_in), "ok");
    assert!(all_in.contains("conbus needs no login right now"));
    assert!(all_in.contains("conbus에 지금 필요한 로그인은 없습니다"));

    // `gh` signed out: a hop's pull request lookup needs it, and this page
    // used to stay green.
    let (_gh_out_dir, gh_out) = fake_gh(1);
    let gh_signed_out = page_with(&[
        ("CONBUS_CODEX_BIN", &codex),
        ("CONBUS_AGY_BIN", &agy),
        ("CONBUS_GH", &gh_out),
    ]);
    assert_eq!(banner(&gh_signed_out), "warn");
    assert!(gh_signed_out.contains("<td>not signed in</td>"));

    let (_agy_out_dir, agy_out, _calls) = fake_logged_out_agy();
    let agy_signed_out = page_with(&[
        ("CONBUS_CODEX_BIN", &codex),
        ("CONBUS_AGY_BIN", &agy_out),
        ("CONBUS_GH", &gh),
    ]);
    assert_eq!(banner(&agy_signed_out), "warn");

    let (_refused_dir, refused) = codex_refusing_with("authentication required");
    let codex_signed_out = page_with(&[
        ("CONBUS_CODEX_BIN", &refused),
        ("CONBUS_AGY_BIN", &agy),
        ("CONBUS_GH", &gh),
    ]);
    assert_eq!(banner(&codex_signed_out), "warn");
    assert!(codex_signed_out.contains("<td>AUTH_REQUIRED</td>"));
}

#[test]
fn a_vendor_that_is_not_installed_is_neutral() {
    let (_codex_dir, codex) = codex_signed_in();

    // `agy` and `gh` are simply not on this machine.
    let some_missing = page_with(&[("CONBUS_CODEX_BIN", &codex)]);
    assert_eq!(banner(&some_missing), "ok", "{some_missing}");
    for name in ["agy", "gh"] {
        assert!(
            some_missing.contains(&format!("<tr><td>{name}</td><td>not installed</td>")),
            "{name}: {some_missing}"
        );
    }

    let all_missing = page_with(&[]);
    assert_eq!(banner(&all_missing), "ok", "{all_missing}");
    assert!(all_missing.contains("<tr><td>codex</td><td>not installed</td>"));

    let table = the_table();
    for name in ["codex", "agy", "gh"] {
        assert!(row(&table, name).contains("not installed"), "{table}");
    }
}

#[test]
fn a_vendor_that_could_not_be_told_is_neither_green_nor_a_sign_in() {
    let (_refused_dir, broken) = codex_refusing_with("the server is on fire");
    let (_agy_dir, agy) = agy_signed_in();
    let (_gh_dir, gh) = fake_gh(0);

    let html = page_with(&[
        ("CONBUS_CODEX_BIN", &broken),
        ("CONBUS_AGY_BIN", &agy),
        ("CONBUS_GH", &gh),
    ]);

    assert_eq!(banner(&html), "neutral", "{html}");
    assert!(html.contains("SKIP"), "{html}");
}

#[test]
fn a_sign_out_outranks_what_could_not_be_told() {
    let (_refused_dir, broken) = codex_refusing_with("the server is on fire");
    let (_gh_dir, gh_out) = fake_gh(1);

    let html = page_with(&[("CONBUS_CODEX_BIN", &broken), ("CONBUS_GH", &gh_out)]);

    assert_eq!(banner(&html), "warn", "{html}");
}

/// An `open` and an `xdg-open` that only keep their calls.
fn opener() -> (tempfile::TempDir, std::path::PathBuf, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let calls = dir.path().join("calls");
    for name in ["open", "xdg-open"] {
        support::write_executable(
            &dir.path().join(name),
            &format!("#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\n", calls.display()),
        );
    }
    let path = format!(
        "{}:{}",
        dir.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    (dir, calls, path)
}

#[test]
fn only_an_explicit_html_and_open_opens_the_page() {
    let root = tempfile::tempdir().expect("tempdir");
    let page = root.path().join("auth.html");
    let page_arg = page.to_str().expect("utf8");
    let (_dir, calls, path) = opener();

    for args in [
        &["auth"][..],
        &["auth", "--open"],
        &["auth", "--html", page_arg],
        &["auth", "--url", "https://x.example/", "--open"],
        &[
            "auth",
            "--url",
            "https://x.example/",
            "--html",
            page_arg,
            "--open",
        ],
    ] {
        let output = conbus(root.path())
            .args(args)
            .env("PATH", &path)
            .output()
            .expect("run auth");
        assert!(output.status.success(), "{args:?}");
        assert!(!calls.exists(), "{args:?} opened something");
    }

    let output = conbus(root.path())
        .args(["auth", "--html", page_arg, "--open"])
        .env("PATH", &path)
        .output()
        .expect("run auth");
    assert!(output.status.success());
    assert_eq!(
        std::fs::read_to_string(&calls).expect("the page was opened"),
        format!("{page_arg}\n")
    );
}

#[test]
fn an_opener_that_fails_is_said_and_is_not_a_failure() {
    let root = tempfile::tempdir().expect("tempdir");
    let page = root.path().join("auth.html");
    let empty = tempfile::tempdir().expect("tempdir");

    let output = conbus(root.path())
        .args(["auth", "--html", page.to_str().expect("utf8"), "--open"])
        .env("PATH", empty.path())
        .output()
        .expect("run auth");

    assert!(output.status.success());
    assert!(page.exists());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("could not open"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn the_page_lands_owner_only_from_its_first_byte() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().expect("tempdir");
        let page = root.path().join("auth.html");
        std::fs::write(&page, "old").expect("seed");
        std::fs::set_permissions(&page, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        // A second name for the old file: it would show the page if the page
        // were written into that world-readable file before the chmod.
        let other_name = root.path().join("other-name");
        std::fs::hard_link(&page, &other_name).expect("link");

        let output = conbus(root.path())
            .args(["auth", "--html", page.to_str().expect("utf8")])
            .output()
            .expect("run auth --html");

        assert!(output.status.success());
        assert_eq!(std::fs::read_to_string(&other_name).expect("read"), "old");
        let mode = std::fs::metadata(&page).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "{mode:o}");
        let left: Vec<_> = std::fs::read_dir(root.path())
            .expect("list")
            .map(|entry| entry.expect("entry").file_name())
            .filter(|name| name.to_string_lossy().contains(".tmp"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }
}

#[test]
fn the_page_goes_in_a_closed_root_by_default() {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("tempdir");
        let root = home.path().join("fresh");

        let output = conbus(&root)
            .args(["auth", "--html"])
            .output()
            .expect("run");

        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let mode = std::fs::metadata(&root).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        assert!(root.join("auth.html").exists());
    }
}

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
        !html.contains("never opens a sign-in page") && !html.contains("열지 않습니다"),
        "a hop opens vendor apps, so this cannot be promised"
    );
    for phrase in [
        "conbus needs no login",
        "conbus에 지금 필요한 로그인은 없습니다",
        "conbus has no sign-in page of its own",
        "conbus에는 자체 로그인 페이지가 없습니다",
    ] {
        assert!(html.contains(phrase), "banner must carry: {phrase}");
    }
    for network in [
        "fetch(",
        "XMLHttpRequest",
        "WebSocket",
        "EventSource",
        "import(",
        "sendBeacon",
    ] {
        assert!(!html.contains(network), "{network} reaches a network");
    }
    assert!(!html.contains("src=") && !html.contains("href="), "{html}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&page).expect("stat").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the page is owner-only: {mode:o}");
    }
}
