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

use std::io;
use std::time::Duration;

use conbus_core::paths::Paths;

mod ancestry;
mod url;
mod verdict;

use url::{Address, BRIDGE_SCHEME, CALLBACK_PARAMS, PAGES, UNKNOWN_PAGE};

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

/// One line of the `auth` table.
struct Row {
    name: &'static str,
    status: String,
    why: &'static str,
}

fn note(name: &'static str, status: &str) -> Row {
    Row {
        name,
        status: status.into(),
        why: "",
    }
}

/// A vendor this command does not ask, and what conbus runs it for.
fn not_checked(name: &'static str, why: &'static str) -> Row {
    Row {
        name,
        status: "not checked".into(),
        why,
    }
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
    let (status, why) = match crate::quota::read() {
        Ok(_) => ("signed in".to_string(), CODEX_NEEDED_FOR),
        Err(err)
            if err
                .to_string()
                .to_ascii_lowercase()
                .contains("authentication") =>
        {
            ("AUTH_REQUIRED".to_string(), CODEX_NEEDED_FOR)
        }
        Err(err) => (
            format!("SKIP — {}", shown(&err.to_string())),
            CANNOT_BE_TOLD,
        ),
    };
    Row {
        name: "codex",
        status,
        why,
    }
}

/// `agy models` only — never `-p`, which opens OAuth when logged out.
fn agy_row() -> Row {
    let asked = crate::quota::agy_signed_in(&crate::quota::agy_binary(), Duration::from_secs(5));
    let (status, why) = match asked {
        Ok(()) => ("signed in".to_string(), AGY_NEEDED_FOR),
        Err(crate::quota::AgyNotAsked::AuthRequired) => (
            "AUTH_REQUIRED, run `agy` to sign in".to_string(),
            AGY_NEEDED_FOR,
        ),
        Err(crate::quota::AgyNotAsked::Skip(why)) => {
            (format!("SKIP — {}", shown(&why)), AGY_NEEDED_FOR)
        }
    };
    Row {
        name: "agy",
        status,
        why,
    }
}

/// `gh auth status`, five seconds. Exit 0 means a token exists; anything
/// else is reported, never a refusal of the command itself.
fn gh_row() -> Row {
    let bin = std::env::var("CONBUS_GH").unwrap_or_else(|_| "gh".into());
    let mut command = std::process::Command::new(bin);
    command.args(["auth", "status"]);
    let (status, why) = match crate::code::output_within(command, Duration::from_secs(5)) {
        Ok(output) if output.status.success() => ("signed in".to_string(), GH_NEEDED_FOR),
        Ok(_) => ("not signed in".to_string(), GH_NEEDED_FOR),
        Err(err) => (format!("SKIP — {}", shown(&err)), CANNOT_BE_TOLD),
    };
    Row {
        name: "gh",
        status,
        why,
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

/// `conbus auth --html [PATH]`: one self-contained page — banner, the same
/// table, and a paste box that classifies a URL entirely offline.
fn write_html(path: &std::path::Path, rows: &[Row]) -> io::Result<()> {
    let callback_params_json = serde_json::json!(CALLBACK_PARAMS);
    let pages_json = serde_json::Value::Array(
        PAGES
            .iter()
            .map(|page| serde_json::json!([page.scheme, page.host, page.subdomains, page.label]))
            .collect(),
    );
    let mut table = String::new();
    let mut broken = false;
    for row in rows {
        if row.status.contains("AUTH_REQUIRED") || row.status.starts_with("SKIP") {
            broken = true;
        }
        table.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
            row.name, row.status, row.why
        ));
    }
    let (banner_class, banner_en, banner_ko) = if broken {
        (
            "warn",
            "a login below needs the vendor's own sign-in",
            "아래 로그인은 벤더 자체 로그인이 필요합니다",
        )
    } else {
        (
            "ok",
            "conbus needs no login right now",
            "conbus에 지금 필요한 로그인은 없습니다",
        )
    };
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let html = format!(
        r##"<!doctype html>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'">
<title>conbus auth</title>
<style>
body {{ font: 14px/1.5 -apple-system, system-ui, monospace; margin: 2em; color: #1a1a1a; }}
.banner {{ padding: 1em 1.2em; border-radius: 8px; font-size: 17px; font-weight: 600; }}
.banner.ok {{ background: #e3f5e3; border: 1px solid #7cc47c; }}
.banner.warn {{ background: #fdf3d7; border: 1px solid #d9b84a; }}
.banner small {{ display: block; font-weight: 400; margin-top: .3em; }}
.sub {{ color: #555; margin: .6em 0 1.4em; }}
table {{ border-collapse: collapse; margin: 1em 0 2em; }}
td, th {{ text-align: left; padding: .35em 1.4em .35em 0; border-bottom: 1px solid #eee; vertical-align: top; }}
td:first-child {{ font-weight: 600; }}
input[type=text] {{ width: 34em; max-width: 90%; font: inherit; padding: .4em; }}
#verdict {{ white-space: pre-wrap; font-family: ui-monospace, monospace; margin-top: 1em; }}
</style>
<div class="banner {banner_class}">{banner_en}<small>{banner_ko}</small></div>
<p class="sub">conbus never opens a sign-in page on its own · conbus는 스스로 로그인 페이지를 열지 않습니다 — generated {now}</p>
<table>{table}</table>
<h2>a tab opened — paste its URL</h2>
<input type="text" id="url" placeholder="chrome-extension://… or https://…">
<button onclick="look()">whose is it</button>
<div id="verdict"></div>
<script>
var PAGES = {pages_json};
var CALLBACK_PARAMS = {callback_params_json};
function parsed(text) {{
  try {{ return new URL(text); }} catch (e) {{ return null; }}
}}
function pageOf(address) {{
  if (!address) return "unknown page";
  var scheme = address.protocol.slice(0, -1).toLowerCase();
  var host = address.hostname.toLowerCase();
  for (var i = 0; i < PAGES.length; i++) {{
    var page = PAGES[i];
    if (scheme !== page[0]) continue;
    if (host === page[1]) return page[3];
    var dotted = "." + page[1];
    if (page[2] && host.length > dotted.length && host.slice(-dotted.length) === dotted) return page[3];
  }}
  return "unknown page";
}}
function isLoopback(host) {{
  return host === "localhost" || host === "[::1]" || /^127\.\d+\.\d+\.\d+$/.test(host) ||
    /^\[::ffff:7f[0-9a-f]{{2}}:[0-9a-f]{{1,4}}\]$/.test(host);
}}
function callbackOf(value) {{
  var hasScheme = value.indexOf("://") >= 0;
  if (!hasScheme && !/^[^\/?#\\]*:\d+([\/?#]|$)/.test(value)) return null;
  return parsed(hasScheme ? value : "http://" + value);
}}
function look() {{
  var address = parsed(document.getElementById("url").value.trim());
  var ports = [], elsewhere = false;
  if (address) {{
    CALLBACK_PARAMS.forEach(function (name) {{
      address.searchParams.getAll(name).forEach(function (value) {{
        var callback = callbackOf(value);
        if (!callback) return;
        if (!isLoopback(callback.hostname.toLowerCase())) elsewhere = true;
        else if (callback.port && ports.indexOf(callback.port) < 0) ports.push(callback.port);
      }});
    }});
  }}
  var out = "this      " + pageOf(address) + "\n";
  ports.forEach(function (port) {{
    out += "process   lsof -nP -iTCP:" + port + " -sTCP:LISTEN -t   # then: ps -o ppid=,comm= -p <pid>, up to the root\n";
  }});
  if (elsewhere) out += "process   its callback is on another host, so this machine's processes say nothing about it\n";
  if (!ports.length && !elsewhere) out += "process   the URL names no callback to look up\n";
  out += "conbus    this page cannot see processes; pbpaste | conbus auth --url - gives the verdict";
  document.getElementById("verdict").textContent = out;
}}
</script>
"##,
    );
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, html)?;
    // 0600 — the page names local files and must stay owner-only.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// `conbus auth [--url URL] [--html [PATH]] [--open]`.
pub fn run(paths: &Paths, url: Option<&str>, html: Option<String>, open: bool) -> io::Result<()> {
    if let Some(url) = url {
        print!("{}", describe_url(url));
        return Ok(());
    }
    let rows = probe_rows();
    if let Some(path) = html {
        let path = if path.is_empty() {
            paths.root().join("auth.html")
        } else {
            std::path::PathBuf::from(path)
        };
        write_html(&path, &rows)?;
        println!("wrote {}", path.display());
        if open {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let _ = std::process::Command::new(opener)
                .arg(&path)
                .stdin(std::process::Stdio::null())
                .status();
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
