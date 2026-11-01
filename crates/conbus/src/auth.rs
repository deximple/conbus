//! `conbus auth` — when a sign-in tab pops up, name what asked for it.
//!
//! Operators have guessed at conbus every time a vendor's OAuth page opened.
//! One was ours (an unattended `agy -p` probe — since removed); another was
//! Codex.app's Playwright MCP bridge. This command answers three questions:
//! whether conbus needs a login at all (never), what each vendor's state is,
//! and — given the URL that opened — whose page it is and which process asked.
//!
//! It blocks nothing and fixes nothing: it reads state and points at the
//! vendor's own sign-in flow. Exit 0 regardless of vendor state — recording
//! does not depend on any of it.

use std::io;
use std::time::Duration;

use conbus_core::paths::Paths;

mod url;

use url::{Address, BRIDGE_SCHEME, PAGES, UNKNOWN_PAGE};

/// Query parameters that may carry the listen port the page connects back to.
const PORT_PARAMS: &[&str] = &["mcpRelayUrl", "redirect_uri", "redirect_url", "callback"];

/// Vendor config files a URL's `token=` may have been copied from. Their
/// contents are never printed — only which file held the value.
const TOKEN_FILES: &[&str] = &[
    ".codex/config.toml",
    ".cursor/mcp.json",
    ".claude.json",
    ".config/opencode/opencode.json",
];

/// One line of the `auth` table.
struct Row {
    name: &'static str,
    status: String,
    why: &'static str,
}

fn probe_rows() -> Vec<Row> {
    let mut rows = vec![
        Row {
            name: "conbus",
            status: "needs no login of its own".into(),
            why: "and recording needs none",
        },
        Row {
            name: "background",
            status: "nothing conbus runs on its own".into(),
            why: "asks a vendor to sign in",
        },
    ];

    // Codex is asked through its own read: an `authentication` refusal is
    // the logged-out answer, anything else is not a clean no.
    let codex = match crate::quota::read() {
        Ok(_) => ("signed in", "for quota, gap fill, hop"),
        Err(err)
            if err
                .to_string()
                .to_ascii_lowercase()
                .contains("authentication") =>
        {
            ("AUTH_REQUIRED", "for quota, gap fill, hop")
        }
        Err(err) => {
            rows.push(Row {
                name: "codex",
                status: format!("SKIP — {err}"),
                why: "its sign-in could not be told",
            });
            ("", "")
        }
    };
    if !codex.0.is_empty() {
        rows.push(Row {
            name: "codex",
            status: codex.0.into(),
            why: codex.1,
        });
    }

    // `agy models` only — never `-p`, which opens OAuth when logged out.
    let agy = match crate::quota::agy_signed_in(&crate::quota::agy_binary(), Duration::from_secs(5))
    {
        Ok(()) => (
            "signed in",
            "for its quota line only; recording is unaffected",
        ),
        Err(crate::quota::AgyNotAsked::AuthRequired) => (
            "AUTH_REQUIRED",
            "for its quota line only; recording is unaffected; run `agy` to sign in",
        ),
        Err(crate::quota::AgyNotAsked::Skip(why)) => {
            rows.push(Row {
                name: "agy",
                status: format!("SKIP — {why}"),
                why: "its sign-in could not be told",
            });
            ("", "")
        }
    };
    if !agy.0.is_empty() {
        rows.push(Row {
            name: "agy",
            status: agy.0.into(),
            why: agy.1,
        });
    }

    let gh = gh_status();
    rows.push(Row {
        name: "gh",
        status: gh.0,
        why: gh.1,
    });
    rows.extend([
        Row {
            name: "claude",
            status: "not checked".into(),
            why: "conbus runs it only for `summary` and a warm hop",
        },
        Row {
            name: "cursor",
            status: "not used".into(),
            why: "hooks only",
        },
        Row {
            name: "grok",
            status: "not used".into(),
            why: "hooks only",
        },
        Row {
            name: "opencode",
            status: "not used".into(),
            why: "hooks only",
        },
    ]);
    rows
}

/// `gh auth status`, five seconds. Exit 0 means a token exists; anything
/// else is reported, never a refusal of the command itself.
fn gh_status() -> (String, &'static str) {
    let bin = std::env::var("CONBUS_GH").unwrap_or_else(|_| "gh".into());
    let mut command = std::process::Command::new(bin);
    command
        .args(["auth", "status"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match crate::code::output_within(command, Duration::from_secs(5)) {
        Ok(output) if output.status.success() => {
            ("signed in".into(), "for hop's pull request lookup")
        }
        Ok(_) => ("not signed in".into(), "for hop's pull request lookup"),
        Err(err) => (format!("SKIP — {err}"), "its sign-in could not be told"),
    }
}

/// The listen port the page will reach back to, when a known parameter
/// carries one.
fn page_port(address: &Address<'_>) -> Option<u16> {
    for value in address.values(PORT_PARAMS) {
        // First `:` after the last `/` of the scheme — `ws://host:PORT/x`
        // and `http://localhost:PORT/callback` both land the port before
        // the next `/` or end.
        let digits: String = value
            .chars()
            .skip_while(|c| *c != ':')
            .skip(1)
            .skip_while(|c| *c != ':')
            .skip(1)
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if let Ok(port) = digits.parse::<u16>() {
            return Some(port);
        }
    }
    None
}

/// Who is listening on `port`, as an ancestry chain `App > parent > pid-comm`,
/// through `lsof` + `ps`. No arguments are ever printed — process names only.
fn port_ancestry(port: u16) -> Option<(Vec<String>, bool)> {
    let output = std::process::Command::new("lsof")
        .args(["-nP", &format!("-iTCP:{port}"), "-sTCP:LISTEN", "-t"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return Some((Vec::new(), false));
    }
    let pid = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|l| l.trim().parse::<u32>().ok())?;
    let mut chain = Vec::new();
    let mut current = pid;
    for _ in 0..8 {
        let line = std::process::Command::new("ps")
            .args(["-o", "ppid=,comm=", "-p", &current.to_string()])
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output()
            .ok()?;
        let text = String::from_utf8_lossy(&line.stdout);
        let line = text.trim();
        if line.is_empty() {
            break;
        }
        let (ppid, comm) = line.split_once(' ').unwrap_or((line, ""));
        chain.push(comm.trim().to_string());
        let Ok(parent) = ppid.trim().parse::<u32>() else {
            break;
        };
        if parent == 0 || parent == current {
            break;
        }
        current = parent;
    }
    // `comm` can carry the full path; only the binary's own name counts —
    // a *directory* named conbus-* does not make the page ours.
    let ours = chain
        .iter()
        .any(|name| name.rsplit('/').next().unwrap_or(name) == "conbus");
    Some((chain, ours))
}

/// Whether `token` appears in one of the vendor config files — existence
/// only, the value is never printed or kept.
fn token_origin(token: &str) -> String {
    let Some(home) = std::env::var_os("HOME").map(std::path::PathBuf::from) else {
        return "cannot tell — no HOME".into();
    };
    for rel in TOKEN_FILES {
        let file = home.join(rel);
        if std::fs::read_to_string(&file)
            .map(|text| text.contains(token))
            .unwrap_or(false)
        {
            return format!("came from {}", file.display());
        }
    }
    "present in none of the vendor config files conbus checks".into()
}

/// `conbus auth --url <URL>`: whose page, who asked, whether conbus did.
fn describe_url(url: &str) -> String {
    let mut out = String::new();
    let address = Address::parse(url.trim());
    let page = address.as_ref().and_then(Address::page);
    let kind = page.map_or(UNKNOWN_PAGE, |page| page.label);
    out.push_str(&format!("this      {kind}\n"));

    // `client` may be a JSON blob whose `name` is the only safe value to
    // echo — the rest of the query stays unprinted.
    let value_of = |name: &str| {
        address
            .as_ref()
            .and_then(|address| address.values(&[name]).into_iter().next())
    };
    if let Some(client) = value_of("client") {
        let name = serde_json::from_str::<serde_json::Value>(&client)
            .ok()
            .and_then(|v| v.get("name").and_then(|n| n.as_str()).map(str::to_string))
            .unwrap_or_else(|| "a client it does not name".into());
        out.push_str(&format!("asked by  {name}\n"));
    }

    if let Some(token) = value_of("token") {
        out.push_str(&format!("token     {}\n", token_origin(&token)));
    }

    let port = address.as_ref().and_then(page_port);
    let ancestry = port.and_then(port_ancestry);
    match (port, &ancestry) {
        (Some(_), Some((chain, _))) if !chain.is_empty() => {
            out.push_str(&format!("process   {}\n", chain.join(" > ")));
        }
        (Some(port), Some(_)) => {
            out.push_str(&format!("process   no process holds port {port} now\n"));
        }
        (Some(_), None) => {
            out.push_str("process   cannot tell — lsof is not there\n");
        }
        (None, _) => {
            out.push_str("process   no port in the URL to look up\n");
        }
    }

    match &ancestry {
        Some((chain, ours)) if !chain.is_empty() => {
            if *ours {
                out.push_str("conbus    yes: conbus is in this page's process ancestry\n");
            } else {
                out.push_str(
                    "conbus    no: nothing in this page's ancestry is conbus, and conbus starts no server for its reads\n",
                );
            }
        }
        Some(_) => {
            out.push_str("conbus    cannot tell — the process that opened it is gone\n");
        }
        None => {
            out.push_str("conbus    cannot tell — no process to inspect\n");
        }
    }

    if page.is_some_and(|page| page.scheme == BRIDGE_SCHEME) {
        out.push_str(
            "cause     the token in that file differs from the extension's token in the Chrome profile this opened in\n",
        );
        out.push_str(
            "fix       copy the token from the extension's page into that file; PLAYWRIGHT_MCP_PROFILE_DIR_NAME pins the profile\n",
        );
    }
    out
}

/// `conbus auth --html [PATH]`: one self-contained page — banner, the same
/// table, and a paste box that classifies a URL entirely offline.
fn write_html(path: &std::path::Path, rows: &[Row]) -> io::Result<()> {
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
var PORT_PARAMS = ["mcpRelayUrl","redirect_uri","redirect_url","callback"];
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
function look() {{
  var address = parsed(document.getElementById("url").value.trim());
  var kind = pageOf(address), port = "";
  if (address) {{
    PORT_PARAMS.forEach(function (name) {{
      var value = address.searchParams.get(name);
      var m = value && value.match(/^[^:]*:\/\/[^:/]+:(\d+)/);
      if (m) port = m[1];
    }});
  }}
  var out = "this      " + kind + "\n";
  out += port ? "process   lsof -nP -iTCP:" + port + " -sTCP:LISTEN -t   # then: ps -o ppid=,comm= -p <pid>\n"
              : "process   no port in the URL to look up\n";
  out += "conbus    check the chain: any 'conbus' in it means this page is ours (conbus opens none)";
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
        println!("{:<9} {}  {}", row.name, row.status, row.why);
    }
    Ok(())
}
