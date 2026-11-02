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
    // The error reaches this command as JSON, whose escapes stop the first two. A
    // bidirectional override is not escaped by JSON, so it is what would get through.
    let (_codex_dir, codex) = codex_refusing_with("bad\u{1b}[2K\nconbus    no: forged\u{202e}txet");
    let root = tempfile::tempdir().expect("tempdir");

    let output = conbus(root.path())
        .arg("auth")
        .env("CONBUS_CODEX_BIN", &codex)
        .output()
        .expect("run auth");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(row(&stdout, "codex").contains("SKIP"), "{stdout}");
    assert!(!stdout.contains('\u{1b}'), "{stdout:?}");
    assert!(!stdout.contains('\u{202e}'), "{stdout:?}");
    assert!(
        !stdout.lines().any(|l| l.starts_with("conbus    no")),
        "{stdout}"
    );
    let html = page_with(&[("CONBUS_CODEX_BIN", &codex)]);
    assert!(!html.contains('\u{1b}'), "{html:?}");
    assert!(!html.contains('\u{202e}'), "{html:?}");
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
    assert!(
        all_in.contains("nothing it needs is signed out"),
        "{all_in}"
    );
    assert!(
        all_in.contains("필요한 로그인이 빠진 곳은 없습니다"),
        "{all_in}"
    );

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

    let (_refused_dir, refused) =
        codex_refusing_with("codex account authentication required to read rate limits");
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

    // Nothing was asked at all, so nothing can be green.
    let all_missing = page_with(&[]);
    assert_eq!(banner(&all_missing), "neutral", "{all_missing}");
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
fn a_running_vendor_that_quotes_a_missing_file_is_not_called_uninstalled() {
    // Only a program that could not be started is not installed. This one started,
    // answered, and happens to quote the same errno.
    let (_refused_dir, broken) =
        codex_refusing_with("cannot read a file: No such file or directory (os error 2)");

    let html = page_with(&[("CONBUS_CODEX_BIN", &broken)]);

    assert_eq!(banner(&html), "neutral", "{html}");
    assert!(html.contains("<tr><td>codex</td><td>SKIP"), "{html}");
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

    for args in [&["auth"][..], &["auth", "--html", page_arg]] {
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
fn flags_that_cannot_be_honoured_together_are_refused_and_open_nothing() {
    // These used to be ignored without a word: the person asked for a page and got
    // a verdict, or asked for it opened and nothing happened.
    let root = tempfile::tempdir().expect("tempdir");
    let page = root.path().join("page.html");
    let page_arg = page.to_str().expect("utf8");
    let (_dir, calls, path) = opener();

    for (args, says) in [
        (&["auth", "--open"][..], "--open"),
        (&["auth", "--url", "https://x.example/", "--open"], "--url"),
        (
            &["auth", "--url", "https://x.example/", "--html", page_arg],
            "--url",
        ),
        (
            &[
                "auth",
                "--url",
                "https://x.example/",
                "--html",
                page_arg,
                "--open",
            ],
            "--url",
        ),
        (&["auth", "--url", "-", "--html"], "--url"),
    ] {
        let output = conbus(root.path())
            .args(args)
            .env("PATH", &path)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("run auth");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(2), "{args:?}: {stderr}");
        assert!(stderr.contains(says), "{args:?}: {stderr}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{args:?}");
        assert!(!calls.exists(), "{args:?} opened something");
        assert!(!page.exists(), "{args:?} wrote a page");
    }
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
    let (_agy_dir, agy) = agy_signed_in();
    let (_codex_dir, codex) = codex_signed_in();
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
        "nothing it needs is signed out",
        "필요한 로그인이 빠진 곳은 없습니다",
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

#[test]
fn an_api_key_login_is_not_called_signed_out() {
    let (_dir, codex) = codex_refusing_with("chatgpt authentication required to read rate limits");
    let html = page_with(&[("CONBUS_CODEX_BIN", codex.as_path())]);
    assert!(!html.contains("AUTH_REQUIRED"), "{html}");
    assert_ne!(banner(&html), "warn", "{html}");
}

#[test]
fn gh_that_could_not_reach_github_is_not_called_signed_out() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gh");
    support::write_executable(
        &path,
        "#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\necho '  X Timeout trying to log in to github.com account someone' >&2\nexit 1\n",
    );
    warmed(&path);
    let html = page_with(&[("CONBUS_GH", path.as_path())]);
    assert!(!html.contains("not signed in"), "{html}");
}

#[test]
fn a_vendor_conbus_does_not_need_right_now_does_not_turn_the_banner() {
    let (_dir, agy, _calls) = fake_logged_out_agy();
    let html = page_with(&[("CONBUS_AGY_BIN", agy.as_path())]);
    assert_ne!(banner(&html), "warn", "{html}");
}

#[test]
fn only_the_words_of_a_missing_codex_login_are_a_sign_out() {
    for message in [
        "407 Proxy Authentication Required",
        "authentication failed talking to the workspace",
        "chatgpt authentication required to read rate limits",
    ] {
        let (_dir, codex) = codex_refusing_with(message);
        let html = page_with(&[("CONBUS_CODEX_BIN", codex.as_path())]);
        assert!(!html.contains("AUTH_REQUIRED"), "{message}: {html}");
        assert_ne!(banner(&html), "warn", "{message}");
    }

    let (_dir, codex) =
        codex_refusing_with("codex account authentication required to read rate limits");
    let html = page_with(&[("CONBUS_CODEX_BIN", codex.as_path())]);
    assert!(html.contains("<td>AUTH_REQUIRED</td>"), "{html}");
    assert_eq!(banner(&html), "warn", "{html}");
}

/// The page with a signed-in Codex and `gh` saying `json`.
fn page_with_gh_saying(json: &str) -> String {
    let (_codex_dir, codex) = codex_signed_in();
    let (_gh_dir, gh) = gh_saying(json);
    page_with(&[("CONBUS_CODEX_BIN", &codex), ("CONBUS_GH", &gh)])
}

fn gh_account(state: &str, active: bool) -> String {
    format!(
        r#"{{"state":"{state}","active":{active},"host":"github.com","login":"someone","tokenSource":"keyring","token":"gho_NOT_FOR_PRINTING","gitProtocol":"https"}}"#
    )
}

#[test]
fn gh_is_read_account_by_account() {
    // The active account decides. `state` is gh's own word for each.
    for (accounts, want_status, want_banner) in [
        (gh_account("success", true), "signed in", "ok"),
        (gh_account("error", true), "not signed in", "warn"),
        (gh_account("timeout", true), "SKIP", "neutral"),
        (
            format!(
                "{},{}",
                gh_account("success", true),
                gh_account("error", false)
            ),
            "signed in",
            "ok",
        ),
        (
            format!(
                "{},{}",
                gh_account("error", false),
                gh_account("success", true)
            ),
            "signed in",
            "ok",
        ),
        (
            format!(
                "{},{}",
                gh_account("success", true),
                gh_account("timeout", false)
            ),
            "signed in",
            "ok",
        ),
        (gh_account("something new", true), "SKIP", "neutral"),
        (gh_account("success", false), "SKIP", "neutral"),
    ] {
        let html = page_with_gh_saying(&format!(r#"{{"hosts":{{"github.com":[{accounts}]}}}}"#));
        assert!(
            html.contains(&format!("<tr><td>gh</td><td>{want_status}")),
            "{accounts}: {html}"
        );
        assert_eq!(banner(&html), want_banner, "{accounts}");
        assert!(!html.contains("gho_NOT_FOR_PRINTING"), "{html}");
    }
}

#[test]
fn gh_logged_in_nowhere_is_signed_out_and_nonsense_is_not() {
    let html = page_with_gh_saying(r#"{"hosts":{}}"#);
    assert!(html.contains("<tr><td>gh</td><td>not signed in"), "{html}");
    assert_eq!(banner(&html), "warn", "{html}");

    for nonsense in ["", "not json", "[]", r#"{"hosts":7}"#, r#"{"other":{}}"#] {
        let html = page_with_gh_saying(nonsense);
        assert!(
            html.contains("<tr><td>gh</td><td>SKIP"),
            "{nonsense:?}: {html}"
        );
        assert_eq!(banner(&html), "neutral", "{nonsense:?}");
    }
}

#[test]
fn gh_com_decides_when_another_host_disagrees() {
    let enterprise = gh_account("error", true).replace("github.com", "ghe.example");
    let html = page_with_gh_saying(&format!(
        r#"{{"hosts":{{"ghe.example":[{enterprise}],"github.com":[{}]}}}}"#,
        gh_account("success", true)
    ));
    assert!(html.contains("<tr><td>gh</td><td>signed in"), "{html}");

    let enterprise_in = gh_account("success", true).replace("github.com", "ghe.example");
    let html = page_with_gh_saying(&format!(
        r#"{{"hosts":{{"ghe.example":[{enterprise_in}],"github.com":[{}]}}}}"#,
        gh_account("error", true)
    ));
    assert!(html.contains("<tr><td>gh</td><td>not signed in"), "{html}");

    // A machine on an enterprise host alone is judged by that host.
    let html = page_with_gh_saying(&format!(r#"{{"hosts":{{"ghe.example":[{enterprise}]}}}}"#));
    assert!(html.contains("<tr><td>gh</td><td>not signed in"), "{html}");
}

#[test]
fn gh_is_asked_for_its_json() {
    let root = tempfile::tempdir().expect("tempdir");
    let (dir, gh) = fake_gh(0);
    let output = conbus(root.path())
        .arg("auth")
        .env("CONBUS_GH", &gh)
        .output()
        .expect("run auth");
    assert!(output.status.success());
    let calls = std::fs::read_to_string(dir.path().join("calls")).expect("calls");
    assert_eq!(calls, "auth status --json hosts\n");
    assert!(
        !calls.contains("--show-token"),
        "no token is ever asked for"
    );
}

#[test]
fn the_banner_says_what_was_asked_and_what_was_not() {
    let (_codex_dir, codex) = codex_signed_in();
    let (_gh_dir, gh) = fake_gh(0);

    let html = page_with(&[("CONBUS_CODEX_BIN", &codex), ("CONBUS_GH", &gh)]);

    // Automatic hops run `claude -p`, and nothing here asked Claude.
    assert!(!html.contains("conbus needs no login right now"), "{html}");
    assert!(!html.contains("지금 필요한 로그인은 없습니다"), "{html}");
    assert!(html.contains("asked: codex, gh"), "{html}");
    assert!(html.contains("not installed: agy"), "{html}");
    assert!(
        html.contains("not asked: claude, cursor, grok, opencode"),
        "{html}"
    );
}

#[test]
fn the_banner_names_what_it_is_yellow_or_grey_about() {
    let (_codex_dir, codex) = codex_signed_in();
    let (_gh_dir, gh) = fake_gh(1);
    let html = page_with(&[("CONBUS_CODEX_BIN", &codex), ("CONBUS_GH", &gh)]);
    assert!(html.contains("that is missing: gh<"), "{html}");

    let (_broken_dir, broken) = codex_refusing_with("the server is on fire");
    let (_gh_in_dir, gh_in) = fake_gh(0);
    let html = page_with(&[("CONBUS_CODEX_BIN", &broken), ("CONBUS_GH", &gh_in)]);
    assert!(html.contains("could not tell for: codex<"), "{html}");
}

#[test]
fn a_signed_out_agy_is_told_not_to_sign_in_for_nothing() {
    let root = tempfile::tempdir().expect("tempdir");
    let (_dir, agy, _calls) = fake_logged_out_agy();
    let output = conbus(root.path())
        .arg("auth")
        .env("CONBUS_AGY_BIN", &agy)
        .output()
        .expect("run auth");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = row(&stdout, "agy");
    assert!(line.contains("AUTH_REQUIRED"), "{line}");
    assert!(line.contains("only if you use it"), "{line}");
    assert!(!line.contains("run `agy` to sign in"), "{line}");
}

#[test]
fn a_broken_agy_does_not_turn_the_banner_either() {
    let dir = tempfile::tempdir().expect("tempdir");
    let agy = dir.path().join("agy");
    support::write_executable(&agy, "#!/bin/sh\nexit 3\n");
    warmed(&agy);
    let (_codex_dir, codex) = codex_signed_in();
    let (_gh_dir, gh) = fake_gh(0);

    let html = page_with(&[
        ("CONBUS_AGY_BIN", agy.as_path()),
        ("CONBUS_CODEX_BIN", codex.as_path()),
        ("CONBUS_GH", gh.as_path()),
    ]);

    assert_eq!(banner(&html), "ok", "{html}");
    assert!(html.contains("<tr><td>agy</td><td>SKIP"), "{html}");
}
