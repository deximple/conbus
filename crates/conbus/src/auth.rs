//! `conbus auth` — when a sign-in tab pops up, answer with evidence whose it is.
//!
//! Operators have guessed at conbus every time a vendor's sign-in page opened.
//! One was ours (an unattended `agy -p` probe); another was Codex.app's
//! Playwright MCP bridge. This command says whether conbus needs a login at all
//! (never), what the vendors it can ask say, and, given the address that opened,
//! whose page it is and whether conbus is above the process it calls back to. A
//! person must be able to trust the answer rather than guess, so where conbus
//! cannot know, it says `cannot tell`.
//!
//! It blocks nothing and fixes nothing: it reads state and points at the
//! vendor's own sign-in flow. Exit 0 regardless of vendor state — recording
//! does not depend on any of it.

use std::io::{self, BufRead, IsTerminal, Read};
use std::time::Duration;

use conbus_core::paths::Paths;

mod ancestry;
mod page;
mod url;
mod verdict;

use url::{Address, BRIDGE_SCHEME, UNKNOWN_PAGE};

/// What runs with nobody at the keyboard. Automatic overflow lands on Codex or
/// Claude only (`attach::overflow`); the other legs open on a person's order.
const BACKGROUND: &str = "the recorder runs `codex app-server` (gap fill at start; quota and \
`thread/start` on a hop), `claude -p` (a warm hop) and `gh` (a hop's pull request lookup). A hop \
then opens the app it lands on: `open -b` for Codex or Claude and, on a person's order, `cursor \
-n`, or on a Mac `osascript` for a terminal leg. conbus asks for no sign-in of its own; any of \
these vendors may ask for theirs";

const CODEX_NEEDED_FOR: &str = "for quota, gap fill, hop";
const AGY_NEEDED_FOR: &str = "only its quota line needs this: `conbus quota` and `conbus auth` ask \
it, `auth` with `agy models` alone; a hop to it or `conbus run` starts it as a session; recording \
is unaffected";
const GH_NEEDED_FOR: &str = "for hop's pull request lookup";
const CANNOT_BE_TOLD: &str = "its sign-in could not be told";

/// What Codex's app-server says with no account at all. Its error for an API-key
/// login differs by one word, and a proxy's 407 says "authentication" too; neither
/// is a reason to sign in.
const CODEX_SIGNED_OUT: &str = "codex account authentication required";

/// `gh auth status --json hosts` could not be understood.
const GH_UNREADABLE: &str = "gh gave no answer this build can read";

/// A condition, because nothing here saw what the page says or read a token: a
/// URL's token is not compared with anything, and Chrome's storage and the
/// vendors' config files are not opened.
const BRIDGE_CAUSE: &str = "cause     if the page says `Invalid token provided`, the token this \
client was given differs from the extension's in the Chrome profile it opened in\n";
const BRIDGE_FIX: &str = "fix       copy the token from the extension's page into the client's \
`PLAYWRIGHT_MCP_EXTENSION_TOKEN`; `PLAYWRIGHT_MCP_PROFILE_DIR_NAME` pins the Chrome profile\n";

/// What a row of the table means for the `--html` banner.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Standing {
    /// Not a vendor: what the row says of conbus itself.
    Note,
    /// Asked, and nothing conbus needs stands in the way.
    Asked,
    /// Asked, and something conbus needs is signed out.
    SignedOut,
    /// Asked, and something conbus needs could not be told.
    CannotTell,
    /// Not on this machine, so there is nothing to ask.
    NotInstalled,
    /// Never asked: this says nothing either way.
    NotChecked,
}

/// One line of the `auth` table.
struct Row {
    name: &'static str,
    status: String,
    why: &'static str,
    standing: Standing,
}

fn note(name: &'static str, status: &str) -> Row {
    Row {
        name,
        status: status.into(),
        why: "",
        standing: Standing::Note,
    }
}

/// A vendor this command does not ask, and what conbus runs it for.
fn not_checked(name: &'static str, why: &'static str) -> Row {
    Row {
        name,
        status: "not checked".into(),
        why,
        standing: Standing::NotChecked,
    }
}

/// What asking a vendor came to.
enum Answer {
    SignedIn,
    /// With the words for it.
    SignedOut(&'static str),
    /// With why it could not be told.
    Failed(String),
}

/// `conbus_needs` is whether a feature depends on this vendor. One that does not
/// says what it found in its row and leaves the banner alone: a person should not
/// sign in for nothing.
fn asked_row(
    name: &'static str,
    needed_for: &'static str,
    answer: Answer,
    conbus_needs: bool,
) -> Row {
    let if_needed = |standing| {
        if conbus_needs {
            standing
        } else {
            Standing::Asked
        }
    };
    let (status, why, standing) = match answer {
        Answer::SignedIn => ("signed in".to_string(), needed_for, Standing::Asked),
        Answer::SignedOut(words) => (
            words.to_string(),
            needed_for,
            if_needed(Standing::SignedOut),
        ),
        Answer::Failed(failure) if not_installed(&failure) => (
            "not installed".to_string(),
            needed_for,
            Standing::NotInstalled,
        ),
        Answer::Failed(failure) => (
            format!("SKIP — {}", shown(&failure)),
            CANNOT_BE_TOLD,
            if_needed(Standing::CannotTell),
        ),
    };
    Row {
        name,
        status,
        why,
        standing,
    }
}

/// The program could not be started because it is not there: ENOENT at spawn,
/// in the words `AppServer::start` and `code::output_within` put it. An answer
/// from a running vendor that happens to quote that errno does not count.
fn not_installed(failure: &str) -> bool {
    failure.contains("(os error 2)")
        && (failure.starts_with("could not run ") || failure.contains(" unavailable ("))
}

fn probe_rows() -> Vec<Row> {
    vec![
        note(
            "conbus",
            "needs no login of its own, and recording needs none",
        ),
        note("background", BACKGROUND),
        note("measured", verdict::MEASURED),
        codex_row(),
        agy_row(),
        gh_row(),
        not_checked(
            "claude",
            "a warm hop and `summary` run `claude -p`, and `conbus run` starts it",
        ),
        not_checked(
            "cursor",
            "hooks, and `cursor -n` opens it when a hop lands there; `conbus run` starts its agent",
        ),
        not_checked(
            "grok",
            "hooks, and on a Mac `osascript` opens it when a hop lands there; `conbus run` starts it",
        ),
        not_checked(
            "opencode",
            "hooks, and on a Mac `osascript` opens it when a hop lands there; `conbus run` starts it; `doctor` runs `opencode --version`",
        ),
    ]
}

/// Codex is asked through its own read. Only its words for no account are a
/// sign-out; anything else is not a clean no.
fn codex_row() -> Row {
    let answer = match crate::quota::read() {
        Ok(_) => Answer::SignedIn,
        Err(err) if err.to_string().contains(CODEX_SIGNED_OUT) => {
            Answer::SignedOut("AUTH_REQUIRED")
        }
        Err(err) => Answer::Failed(err.to_string()),
    };
    asked_row("codex", CODEX_NEEDED_FOR, answer, true)
}

/// `agy models` only — never `-p`, which opens OAuth when logged out. Nothing
/// conbus needs depends on it, so whatever it says, the banner stays out of it.
fn agy_row() -> Row {
    let answer =
        match crate::quota::agy_signed_in(&crate::quota::agy_binary(), Duration::from_secs(5)) {
            Ok(()) => Answer::SignedIn,
            Err(crate::quota::AgyNotAsked::AuthRequired) => {
                Answer::SignedOut("AUTH_REQUIRED, sign in with `agy` only if you use it")
            }
            Err(crate::quota::AgyNotAsked::Skip(why)) => Answer::Failed(why),
        };
    asked_row("agy", AGY_NEEDED_FOR, answer, false)
}

/// `gh auth status --json hosts`, five seconds. With `--json` it exits 0 whatever
/// the accounts' state, and says for each whether it worked, so a timeout is not a
/// sign-out and another account's bad token is not this one's.
fn gh_row() -> Row {
    let bin = std::env::var("CONBUS_GH").unwrap_or_else(|_| "gh".into());
    let mut command = std::process::Command::new(bin);
    command.args(["auth", "status", "--json", "hosts"]);
    let answer = match crate::code::output_within(command, Duration::from_secs(5)) {
        Ok(output) => gh_answer(&output.stdout),
        Err(err) => Answer::Failed(err),
    };
    asked_row("gh", GH_NEEDED_FOR, answer, true)
}

/// The active account decides, on github.com if it has one there, because that is
/// where a hop's lookup goes. Only `state` and `active` are read.
fn gh_answer(stdout: &[u8]) -> Answer {
    let unreadable = || Answer::Failed(GH_UNREADABLE.to_string());
    let parsed = serde_json::from_slice::<serde_json::Value>(stdout).ok();
    let Some(hosts) = parsed
        .as_ref()
        .and_then(|value| value.get("hosts")?.as_object())
    else {
        return unreadable();
    };
    if hosts.is_empty() {
        return Answer::SignedOut("not signed in");
    }
    let accounts: Vec<&serde_json::Value> = match hosts.get("github.com") {
        Some(host) => vec![host],
        None => hosts.values().collect(),
    };
    let active: Vec<&str> = accounts
        .iter()
        .filter_map(|host| host.as_array())
        .flatten()
        .filter(|account| account.get("active").and_then(|a| a.as_bool()) == Some(true))
        .filter_map(|account| account.get("state")?.as_str())
        .collect();
    if active.contains(&"success") {
        Answer::SignedIn
    } else if active.contains(&"timeout") {
        Answer::Failed("gh could not reach GitHub in time".to_string())
    } else if active.contains(&"error") {
        Answer::SignedOut("not signed in")
    } else {
        unreadable()
    }
}

/// What of `text` may reach a terminal: printable characters, and `?` for the
/// rest. Vendor error text and process names come from outside.
fn shown(text: &str) -> String {
    text.chars()
        .map(|c| if printable(c) { c } else { '?' })
        .collect()
}

/// ASCII that prints, and letters and digits beyond it. Nothing else: no control
/// character, no line break, no bidirectional override.
fn printable(c: char) -> bool {
    matches!(c, ' '..='~') || (!c.is_ascii() && c.is_alphanumeric())
}

/// More than any address a browser keeps, and a limit on what a mistaken pipe
/// can make this read.
const STDIN_MAX_BYTES: u64 = 1024 * 1024;

/// `--url -`. Off the command line, a token reaches no shell history and no recorded
/// tool call.
fn url_from_stdin() -> io::Result<String> {
    let stdin = io::stdin();
    if stdin.is_terminal() {
        first_line_of(stdin.lock())
    } else {
        the_one_line_of(stdin.lock())
    }
}

fn no_url_on_stdin() -> io::Error {
    io::Error::other("no URL on stdin; try pbpaste | conbus auth --url -")
}

/// A pipe: all of it, which must be one line. An address has no line breaks in it,
/// and a browser given one with a break joins the parts, so the first line alone
/// may name a different host from the page that opened.
fn the_one_line_of(reader: impl Read) -> io::Result<String> {
    let mut bytes = Vec::new();
    reader.take(STDIN_MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > STDIN_MAX_BYTES {
        return Err(io::Error::other(
            "stdin is longer than any address; nothing was looked up",
        ));
    }
    let text = String::from_utf8_lossy(&bytes);
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    match lines.as_slice() {
        [] => Err(no_url_on_stdin()),
        [line] => Ok((*line).to_string()),
        _ => Err(io::Error::other(
            "stdin holds more than one line, and an address is one line; nothing was looked up",
        )),
    }
}

/// A terminal: the first line that is not blank, so a pasted address needs only
/// Return. Waiting for end of input here would look like a hang.
fn first_line_of(reader: impl BufRead) -> io::Result<String> {
    let mut reader = reader.take(STDIN_MAX_BYTES);
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(no_url_on_stdin());
        }
        let text = String::from_utf8_lossy(&line);
        if !text.trim().is_empty() {
            return Ok(text.trim().to_string());
        }
    }
}

/// The command line has already gone where it goes. All that is left is to say
/// so, and not to print what it held.
fn warn_about_history(url: &str) {
    if url.contains(['?', '#']) {
        eprintln!(
            "conbus auth: this URL's query is now in your shell history, and in the record of any agent that ran this; next time use: pbpaste | conbus auth --url -"
        );
    }
}

/// `conbus auth --url <URL>`: whose page, who asked, whether conbus did.
fn describe_url(url: &str) -> String {
    let mut out = String::new();
    let address = Address::parse(url.trim());
    let page = address.as_ref().and_then(Address::page);
    let kind = page.map_or(UNKNOWN_PAGE, |page| page.label);
    out.push_str(&format!("this      {kind}\n"));

    let value_of = |name: &str| {
        address
            .as_ref()
            .and_then(|address| address.values(&[name]).into_iter().next())
    };
    if let Some(client) = value_of("client") {
        out.push_str(&format!("url says  {}\n", url::client_claim(&client)));
    }

    let callbacks = address.as_ref().map(Address::callbacks).unwrap_or_default();
    let conbus_runs_it = page.is_some_and(|page| page.conbus_runs_it);
    for line in verdict::describe(&callbacks, conbus_runs_it) {
        out.push_str(&line);
        out.push('\n');
    }

    if page.is_some_and(|page| page.scheme == BRIDGE_SCHEME) {
        out.push_str(BRIDGE_CAUSE);
        out.push_str(BRIDGE_FIX);
    }
    out
}

/// `--open`, the one thing here that opens anything, and only on that flag. A
/// page that would not open is still written, so it says where it is.
fn open_page(path: &std::path::Path) {
    let opener = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let opened = std::process::Command::new(opener)
        .arg(path)
        .stdin(std::process::Stdio::null())
        .status();
    let failed = match opened {
        Ok(status) if status.success() => return,
        Ok(status) => status.to_string(),
        Err(err) => err.to_string(),
    };
    eprintln!(
        "conbus auth: could not open {} with {opener}: {failed}",
        path.display()
    );
}

/// `conbus auth [--url URL] [--html [PATH]] [--open]`.
pub fn run(paths: &Paths, url: Option<&str>, html: Option<String>, open: bool) -> io::Result<()> {
    if let Some(url) = url {
        let answer = if url == "-" {
            describe_url(&url_from_stdin()?)
        } else {
            warn_about_history(url);
            describe_url(url)
        };
        print!("{answer}");
        return Ok(());
    }
    let rows = probe_rows();
    if let Some(path) = html {
        let path = if path.is_empty() {
            paths.keep_private()?;
            paths.root().join("auth.html")
        } else {
            std::path::PathBuf::from(path)
        };
        page::write(&path, &rows)?;
        println!("wrote {}", path.display());
        if open {
            open_page(&path);
        }
        return Ok(());
    }
    for row in &rows {
        println!(
            "{}",
            format!("{:<10} {}  {}", row.name, row.status, row.why).trim_end()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_a_terminal_the_first_line_that_is_not_blank_is_enough() {
        let read = |input: &[u8]| first_line_of(input).map_err(|err| err.to_string());
        assert_eq!(
            read(b"\n  \nhttps://x.example/\nmore\n").unwrap(),
            "https://x.example/"
        );
        assert_eq!(read(b"https://x.example/").unwrap(), "https://x.example/");
        assert!(read(b"\n\n").unwrap_err().contains("no URL"));
    }

    #[test]
    fn from_a_pipe_it_must_be_one_line() {
        let read = |input: &[u8]| the_one_line_of(input).map_err(|err| err.to_string());
        assert_eq!(
            read(b"\n https://x.example/ \n\n").unwrap(),
            "https://x.example/"
        );
        assert!(read(b"\n\n").unwrap_err().contains("no URL"));
        assert!(read(b"a\nb\n").unwrap_err().contains("more than one line"));
    }
}
