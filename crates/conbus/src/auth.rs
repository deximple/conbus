//! `conbus auth` — when a sign-in tab pops up, answer with evidence whose it is.
//!
//! Operators have guessed at conbus every time a vendor's sign-in page opened.
//! One was ours (an unattended `agy -p` probe); another was Codex.app's
//! Playwright MCP bridge. This command says whether conbus needs a login at all
//! (never), what each vendor's state is, and, given the address that opened,
//! whose page it is and whether conbus is above the process it calls back to. An
//! answer with nothing behind it is worse than none, so it says `cannot tell`
//! whenever it did not check.
//!
//! It blocks nothing and fixes nothing: it reads state and points at the
//! vendor's own sign-in flow. Exit 0 regardless of vendor state — recording
//! does not depend on any of it.

use std::io::{self, BufRead, Read};
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
const AGY_NEEDED_FOR: &str = "`conbus quota` and `conbus auth` ask it, `auth` with `agy models` \
alone; a hop to it or `conbus run` starts it as a session; recording is unaffected";
const GH_NEEDED_FOR: &str = "for hop's pull request lookup";
const CANNOT_BE_TOLD: &str = "its sign-in could not be told";

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
    /// Nothing about it stands in the way: signed in, not checked, or not on
    /// this machine at all.
    Clear,
    /// A feature that needs it is signed out.
    SignedOut,
    /// It is there, and it could not be told.
    CannotTell,
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
        standing: Standing::Clear,
    }
}

/// A vendor this command does not ask, and what conbus runs it for.
fn not_checked(name: &'static str, why: &'static str) -> Row {
    Row {
        name,
        status: "not checked".into(),
        why,
        standing: Standing::Clear,
    }
}

/// What asking a vendor came to.
enum Asked {
    SignedIn,
    /// With the words for it.
    SignedOut(&'static str),
    /// With why it could not be told.
    Failed(String),
}

fn asked_row(name: &'static str, needed_for: &'static str, asked: Asked) -> Row {
    let (status, why, standing) = match asked {
        Asked::SignedIn => ("signed in".to_string(), needed_for, Standing::Clear),
        Asked::SignedOut(words) => (words.to_string(), needed_for, Standing::SignedOut),
        Asked::Failed(failure) if not_installed(&failure) => {
            ("not installed".to_string(), needed_for, Standing::Clear)
        }
        Asked::Failed(failure) => (
            format!("SKIP — {}", shown(&failure)),
            CANNOT_BE_TOLD,
            Standing::CannotTell,
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

/// Codex is asked through its own read: an `authentication` refusal is the
/// logged-out answer, anything else is not a clean no.
fn codex_row() -> Row {
    let asked = match crate::quota::read() {
        Ok(_) => Asked::SignedIn,
        Err(err)
            if err
                .to_string()
                .to_ascii_lowercase()
                .contains("authentication") =>
        {
            Asked::SignedOut("AUTH_REQUIRED")
        }
        Err(err) => Asked::Failed(err.to_string()),
    };
    asked_row("codex", CODEX_NEEDED_FOR, asked)
}

/// `agy models` only — never `-p`, which opens OAuth when logged out.
fn agy_row() -> Row {
    let asked =
        match crate::quota::agy_signed_in(&crate::quota::agy_binary(), Duration::from_secs(5)) {
            Ok(()) => Asked::SignedIn,
            Err(crate::quota::AgyNotAsked::AuthRequired) => {
                Asked::SignedOut("AUTH_REQUIRED, run `agy` to sign in")
            }
            Err(crate::quota::AgyNotAsked::Skip(why)) => Asked::Failed(why),
        };
    asked_row("agy", AGY_NEEDED_FOR, asked)
}

/// `gh auth status`, five seconds. Exit 0 means a token exists; anything
/// else is reported, never a refusal of the command itself.
fn gh_row() -> Row {
    let bin = std::env::var("CONBUS_GH").unwrap_or_else(|_| "gh".into());
    let mut command = std::process::Command::new(bin);
    command.args(["auth", "status"]);
    let asked = match crate::code::output_within(command, Duration::from_secs(5)) {
        Ok(output) if output.status.success() => Asked::SignedIn,
        Ok(_) => Asked::SignedOut("not signed in"),
        Err(err) => Asked::Failed(err),
    };
    asked_row("gh", GH_NEEDED_FOR, asked)
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

/// `--url -`: the first line that is not blank. It stops at that line, so a URL
/// pasted at a terminal needs only Return. Off the command line, a token reaches
/// no shell history and no recorded tool call.
fn url_from_stdin() -> io::Result<String> {
    let mut reader = io::stdin().lock().take(STDIN_MAX_BYTES);
    loop {
        let mut line = Vec::new();
        if reader.read_until(b'\n', &mut line)? == 0 {
            return Err(io::Error::other(
                "no URL on stdin; try pbpaste | conbus auth --url -",
            ));
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
    for line in verdict::describe(&callbacks) {
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
