//! `conbus auth`: the vendor table with stand-ins, `--url` naming a page
//! without ever echoing its query values, and `--html` writing one
//! self-contained page that connects nowhere.

use std::process::Command;

#[path = "../support/mod.rs"]
mod support;

mod page;
mod page_script;
mod stdin;
mod table;
mod token;
mod urls;
mod verdict;

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

/// Run a stand-in once before the command under test does. A fresh script's first
/// run is the slow one, and under load it has taken seconds. `gh` and `agy` get
/// five before they read as could not be told, which would be this test's machine
/// speaking and not the code.
fn warmed(path: &std::path::Path) {
    let _ = Command::new(path)
        .arg("--warm")
        .stdin(std::process::Stdio::null())
        .output();
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
    warmed(&path);
    (dir, path, calls)
}

/// A `gh` whose `auth status --json hosts` prints `json`, exiting 0 as the real one
/// does with `--json` whatever the accounts' state. It also keeps its arguments.
fn gh_saying(json: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("gh");
    let calls = dir.path().join("calls");
    support::write_executable(
        &path,
        &format!(
            "#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\nprintf '%s\\n' \"$*\" >>'{}'\ncat <<'JSON'\n{json}\nJSON\n",
            calls.display()
        ),
    );
    warmed(&path);
    (dir, path)
}

/// `gh` with one active account on github.com: signed in when `exit_code` is 0,
/// and otherwise with a token GitHub refuses.
fn fake_gh(exit_code: i32) -> (tempfile::TempDir, std::path::PathBuf) {
    let state = if exit_code == 0 { "success" } else { "error" };
    gh_saying(&format!(
        r#"{{"hosts":{{"github.com":[{{"state":"{state}","active":true,"host":"github.com","login":"someone","tokenSource":"keyring","gitProtocol":"https"}}]}}}}"#
    ))
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

fn verdict_of(stdout: &str) -> &str {
    let lines: Vec<&str> = stdout
        .lines()
        .filter(|l| l.starts_with("conbus "))
        .collect();
    assert_eq!(lines.len(), 1, "one verdict line: {stdout}");
    lines[0]
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

/// A bridge page with a token, and every other value of its query.
const BRIDGE_WITH_TOKEN: &str = "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html?mcpRelayUrl=ws%3A%2F%2F127.0.0.1%3A1%2F&token=TOK3N-USED-FOR-TEST";
