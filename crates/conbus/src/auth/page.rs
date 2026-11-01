//! `conbus auth --html [PATH]`: one self-contained page. The banner, the table,
//! and a paste box whose script names a page entirely offline. No server, and a
//! policy that lets it load nothing and reach nothing.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use conbus_core::clock::Timestamp;

use super::url::{CALLBACK_PARAMS, PAGES};
use super::{Row, Standing};

/// Everything that comes from outside passes through here: a vendor's error text
/// is the vendor's to write, and the page allows inline script.
fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(c),
        }
    }
    out
}

/// The banner's class and its two languages. Yellow for a feature signed out,
/// grey when a vendor could not be told, green only when neither is so. A vendor
/// that is not installed changes none of this: nothing of it is signed out.
fn banner(rows: &[Row]) -> (&'static str, &'static str, &'static str) {
    let any = |standing| rows.iter().any(|row| row.standing == standing);
    if any(Standing::SignedOut) {
        (
            "warn",
            "a login below needs the vendor's own sign-in",
            "아래 로그인은 벤더 자체 로그인이 필요합니다",
        )
    } else if any(Standing::CannotTell) {
        (
            "neutral",
            "conbus could not tell for every vendor below",
            "아래 벤더 중 conbus가 확인하지 못한 곳이 있습니다",
        )
    } else {
        (
            "ok",
            "conbus needs no login right now",
            "conbus에 지금 필요한 로그인은 없습니다",
        )
    }
}

fn render(rows: &[Row]) -> String {
    let callback_params_json = serde_json::json!(CALLBACK_PARAMS);
    let pages_json = serde_json::Value::Array(
        PAGES
            .iter()
            .map(|page| serde_json::json!([page.scheme, page.host, page.subdomains, page.label]))
            .collect(),
    );
    let table: String = rows
        .iter()
        .map(|row| {
            format!(
                "<tr><td>{}</td><td>{}</td><td>{}</td></tr>\n",
                escape(row.name),
                escape(&row.status),
                escape(row.why)
            )
        })
        .collect();
    let (banner_class, banner_en, banner_ko) = banner(rows);
    let now = Timestamp::now();
    format!(
        r##"<!doctype html>
<meta charset="utf-8">
<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'">
<title>conbus auth</title>
<style>
body {{ font: 14px/1.5 -apple-system, system-ui, monospace; margin: 2em; color: #1a1a1a; }}
.banner {{ padding: 1em 1.2em; border-radius: 8px; font-size: 17px; font-weight: 600; }}
.banner.ok {{ background: #e3f5e3; border: 1px solid #7cc47c; }}
.banner.warn {{ background: #fdf3d7; border: 1px solid #d9b84a; }}
.banner.neutral {{ background: #eeeeee; border: 1px solid #bbbbbb; }}
.banner small {{ display: block; font-weight: 400; margin-top: .3em; }}
.sub {{ color: #555; margin: .6em 0 1.4em; }}
table {{ border-collapse: collapse; margin: 1em 0 2em; }}
td, th {{ text-align: left; padding: .35em 1.4em .35em 0; border-bottom: 1px solid #eee; vertical-align: top; }}
td:first-child {{ font-weight: 600; }}
input[type=text] {{ width: 34em; max-width: 90%; font: inherit; padding: .4em; }}
#verdict {{ white-space: pre-wrap; font-family: ui-monospace, monospace; margin-top: 1em; }}
</style>
<div class="banner {banner_class}">{banner_en}<small>{banner_ko}</small></div>
<p class="sub">conbus has no sign-in page of its own; a vendor program it starts may ask for that vendor's, and a row marked not checked was not asked · conbus에는 자체 로그인 페이지가 없습니다. conbus가 실행한 벤더 프로그램은 그 벤더의 로그인을 요구할 수 있고, not checked 행은 묻지 않았습니다 — generated {now}</p>
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
    )
}

/// Written beside and renamed, created 0600: no byte of it ever sits in a file
/// someone else can read, whatever was at `path` before.
pub(super) fn write(path: &Path, rows: &[Row]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut beside = path.as_os_str().to_os_string();
    beside.push(format!(".{}.tmp", std::process::id()));
    let beside = PathBuf::from(beside);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let html = render(rows);
    let written = options
        .open(&beside)
        .and_then(|mut file| file.write_all(html.as_bytes()))
        .and_then(|()| std::fs::rename(&beside, path));
    if written.is_err() {
        let _ = std::fs::remove_file(&beside);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_character_html_gives_a_meaning_is_escaped() {
        assert_eq!(
            escape(r#"<a href="x" id='y'>&</a>"#),
            "&lt;a href=&quot;x&quot; id=&#39;y&#39;&gt;&amp;&lt;/a&gt;"
        );
        assert_eq!(
            escape("plain \u{d55c}\u{ae00} text"),
            "plain \u{d55c}\u{ae00} text"
        );
    }
}
