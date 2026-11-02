//! The `--html` page's own script, run in node against a stub of the page.

use super::urls::{NOT_THE_VENDOR, THE_VENDOR};
use super::*;

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
fn the_pages_script_looks_up_only_a_port_on_this_machine() {
    let (_root, page) = a_page();
    let page_says = |url: &str| page_script_says(&page, &[url]).remove(0);

    for (value, port) in [
        ("http%3A%2F%2F127.0.0.1%3A59999%2Fcb", 59999),
        ("http%3A%2F%2Flocalhost%3A59999%2Fcb", 59999),
        ("http%3A%2F%2F%5B%3A%3A1%5D%3A59999%2Fcb", 59999),
        ("localhost%3A59999%2Fcb", 59999),
        ("http%3A%2F%2Fuser%3A1234%40127.0.0.1%3A9%2Fcb", 9),
    ] {
        let said = page_says(&format!(
            "https://accounts.google.com/o/oauth2/auth?redirect_uri={value}&token=SECRET3"
        ));
        assert!(said.contains(&format!("-iTCP:{port} ")), "{value}: {said}");
        assert!(!said.contains("SECRET3"), "{said}");
    }

    for value in [
        "https%3A%2F%2Fother.example%3A1%2Fcb",
        "http%3A%2F%2F127.0.0.1.other.example%3A1%2F",
        "http%3A%2F%2F10.0.0.1%3A1%2F",
    ] {
        let said = page_says(&format!("https://evil.example/?redirect_uri={value}"));
        assert!(!said.contains("lsof"), "{value}: {said}");
        assert!(said.contains("another host"), "{value}: {said}");
        assert!(!said.contains("other.example"), "{said}");
    }

    let said = page_says("https://example.net/?q=<img src=x onerror=alert(1)>&token=SECRET1");
    assert!(
        !said.contains("onerror") && !said.contains("SECRET1"),
        "{said}"
    );

    // The page cannot see processes, so it gives no verdict of its own.
    let said = page_says("https://accounts.google.com/");
    assert!(!said.contains("conbus opens none"), "{said}");
    assert!(said.contains("conbus auth --url -"), "{said}");
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
fn the_pages_script_says_what_each_kind_of_callback_is() {
    let (_root, page) = a_page();
    let page_says = |value: &str| {
        page_script_says(
            &page,
            &[&format!("https://x.example/?redirect_uri={value}")],
        )
        .remove(0)
    };

    // A port the browser's parser would hide because it is the scheme's default.
    assert!(page_says("http%3A%2F%2Flocalhost%3A80%2F").contains("-iTCP:80 "));
    assert!(page_says("https%3A%2F%2Flocalhost%3A443%2F").contains("-iTCP:443 "));
    assert!(page_says("http%3A%2F%2Flocalhost%2F").contains("names this machine but no port"));

    for value in [
        "cursor%3A%2F%2Fanysphere.cursor-retrieval%2Foauth",
        "http%3A%2F%2F0.0.0.0%3A1%2F",
        "http%3A%2F%2F%5B%3A%3A%5D%3A1%2F",
        "http%3A%2F%2Flocalhost%3A99999%2F",
        "someFunction",
        "",
    ] {
        let said = page_says(value);
        assert!(!said.contains("another host"), "{value:?}: {said}");
        assert!(!said.contains("names no callback"), "{value:?}: {said}");
        assert!(!said.contains("lsof"), "{value:?}: {said}");
    }
    assert!(page_says("cursor%3A%2F%2Fx").contains("custom scheme"));
    assert!(page_says("someFunction").contains("does not read"));
}

#[test]
fn the_pages_script_reads_a_callback_under_another_spelling() {
    let (_root, page) = a_page();
    for name in [
        "redirectUri",
        "callbackUrl",
        "REDIRECT-URI",
        "returnTo",
        "relay_url",
    ] {
        let said = page_script_says(
            &page,
            &[&format!(
                "https://x.example/?{name}=http%3A%2F%2F127.0.0.1%3A59999%2F"
            )],
        )
        .remove(0);
        assert!(said.contains("-iTCP:59999 "), "{name}: {said}");
    }
}
