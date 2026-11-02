//! What `conbus auth` reads out of an address somebody pasted.
//!
//! An address is hostile input, and its query can hold a token. So nothing here
//! matches on a path, a query or a substring; only the values asked for by name
//! are decoded; and whatever does not parse cleanly fails closed, as an `unknown
//! page` or as no callback. A browser's parser accepts more than this one, so
//! the two can only disagree in that direction.

/// A page this command can name. Only `scheme` and `host` decide it.
pub(super) struct Page {
    pub scheme: &'static str,
    pub host: &'static str,
    /// For a domain the vendor owns whole: its subdomains count too.
    pub subdomains: bool,
    /// conbus starts this vendor's program as its own child, so a sign-in it asks
    /// for can call back to a process under conbus: `codex app-server` and the
    /// bridge its MCP servers open, `agy`, `claude -p`. Not `gh`, which conbus asks
    /// for lookups only, and not Cursor or Grok, which a hop opens but does not parent.
    pub conbus_runs_it: bool,
    pub label: &'static str,
}

pub(super) const UNKNOWN_PAGE: &str = "unknown page";

/// The scheme of the one page that is not a website.
pub(super) const BRIDGE_SCHEME: &str = "chrome-extension";

/// One table, for the command and for the `--html` page's script. A label says
/// what the host is, because the path is not looked at: `github.com` is where
/// `gh` signs in, and every other page of that site too. Hosts come from the
/// programs themselves: `platform.claude.com` and `claude.com/cai` are in the Claude
/// Code binary, `antigravity.google` in agy's. Nothing is added from memory, so a
/// sign-in moved elsewhere reads `unknown page` rather than a guess.
pub(super) const PAGES: &[Page] = &[
    Page {
        scheme: BRIDGE_SCHEME,
        host: "mmlmfjhmonkocbjadbfplnigmagldckm",
        subdomains: false,
        conbus_runs_it: true,
        label: "Playwright MCP Bridge connect page (a Chrome extension)",
    },
    Page {
        scheme: "https",
        host: "accounts.google.com",
        subdomains: false,
        conbus_runs_it: true,
        label: "Google sign-in (agy and other Google-backed tools)",
    },
    Page {
        scheme: "https",
        host: "auth.openai.com",
        subdomains: false,
        conbus_runs_it: true,
        label: "OpenAI sign-in (Codex)",
    },
    Page {
        scheme: "https",
        host: "claude.ai",
        subdomains: true,
        conbus_runs_it: true,
        label: "a claude.ai page (Claude Code signs in here)",
    },
    Page {
        scheme: "https",
        host: "claude.com",
        subdomains: true,
        conbus_runs_it: true,
        label: "a claude.com page (Claude Code signs in here)",
    },
    Page {
        scheme: "https",
        host: "antigravity.google",
        subdomains: true,
        conbus_runs_it: true,
        label: "an antigravity.google page (agy signs in here)",
    },
    Page {
        scheme: "https",
        host: "console.anthropic.com",
        subdomains: false,
        conbus_runs_it: true,
        label: "Anthropic Console (Claude Code signs in here)",
    },
    Page {
        scheme: "https",
        host: "cursor.com",
        subdomains: true,
        conbus_runs_it: false,
        label: "a cursor.com page (Cursor signs in here)",
    },
    Page {
        scheme: "https",
        host: "x.ai",
        subdomains: true,
        conbus_runs_it: false,
        label: "an x.ai page (Grok signs in here)",
    },
    Page {
        scheme: "https",
        host: "github.com",
        subdomains: false,
        conbus_runs_it: false,
        label: "a github.com page (gh signs in here)",
    },
];

impl Page {
    fn serves(&self, address: &Address<'_>) -> bool {
        address.scheme == self.scheme
            && (address.host == self.host || (self.subdomains && subdomain(&address.host, self)))
    }
}

/// What a query parameter's name contains, once lower-cased with its punctuation
/// gone, when it may say where the page calls back to: `redirect_uri`, `redirectUri`,
/// `mcpRelayUrl`, `callback`, `return_to`.
pub(super) const CALLBACK_NAME_PARTS: &[&str] = &[
    "redirect",
    "callback",
    "relayurl",
    "returnto",
    "returnurl",
    "returnuri",
];

/// How an address on this machine looks in a value that has not been decoded.
const THIS_MACHINE: &[&str] = &["localhost", "127.0.0.1", "[::1]", "%5b%3a%3a1%5d"];

/// The schemes a callback to a port is written in. Any other is handed to an app.
const WEB_SCHEMES: &[&str] = &["http", "https", "ws", "wss"];

fn names_a_callback(name: &str) -> bool {
    let plain: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    CALLBACK_NAME_PARTS.iter().any(|part| plain.contains(part))
}

fn mentions_this_machine(raw: &str) -> bool {
    let raw = raw.to_ascii_lowercase();
    THIS_MACHINE.iter().any(|spelling| raw.contains(spelling))
}

/// Where a page calls back to, as far as it matters here: a port on another
/// machine says nothing about this machine's processes. Anything that is not plainly
/// one of these stands in the way of a `no`, rather than being skipped.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Callback {
    Local(u16),
    LocalNoPort,
    Remote,
    /// A custom scheme, which the system hands to an app: no port says which process.
    AppScheme,
    /// Written in a way this parser does not place.
    Unread,
}

/// `ws://127.0.0.1:1234/`, `http://[::1]:1234/cb`, or a bare `localhost:1234/cb`.
/// A value with no scheme counts only with a port: `callback=someFunction` is a
/// name, not an address.
fn callback(value: &str) -> Callback {
    let (rest, has_scheme) = match value.split_once("://") {
        Some((scheme, _)) if !valid_scheme(scheme) => return Callback::Unread,
        Some((scheme, _)) if !WEB_SCHEMES.contains(&scheme.to_ascii_lowercase().as_str()) => {
            return Callback::AppScheme;
        }
        Some((_, rest)) => (rest, true),
        None => (value, false),
    };
    let Some((host, port, _)) = authority(rest) else {
        return Callback::Unread;
    };
    match (place_of(&host), port) {
        (Place::Unplaced, _) => Callback::Unread,
        (_, None) if !has_scheme => Callback::Unread,
        (Place::ThisMachine, Some(port)) => Callback::Local(port),
        (Place::ThisMachine, None) => Callback::LocalNoPort,
        (Place::Elsewhere, _) => Callback::Remote,
    }
}

enum Place {
    ThisMachine,
    Elsewhere,
    Unplaced,
}

/// Exactly `localhost`, or an address that is loopback, is this machine. Nothing
/// that merely starts like one: `127.0.0.1.evil.example` is a name, and so elsewhere.
/// `0.0.0.0` and the spellings a browser reads as an address (`127.1`, `2130706433`)
/// are neither, as far as this parser will say.
fn place_of(host: &str) -> Place {
    use std::net::IpAddr;
    if host == "localhost" {
        return Place::ThisMachine;
    }
    match host.parse::<IpAddr>() {
        Ok(ip) if ip.is_unspecified() => Place::Unplaced,
        Ok(IpAddr::V4(v4)) if v4.is_loopback() => Place::ThisMachine,
        Ok(IpAddr::V6(v6))
            if v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()) =>
        {
            Place::ThisMachine
        }
        Ok(_) => Place::Elsewhere,
        Err(_) if ends_in_a_number(host) => Place::Unplaced,
        Err(_) => Place::Elsewhere,
    }
}

/// A host whose last label is a number is read as an address by a browser.
fn ends_in_a_number(host: &str) -> bool {
    let last = host.rsplit('.').next().unwrap_or(host);
    !last.is_empty()
        && (last.bytes().all(|b| b.is_ascii_digit())
            || last
                .strip_prefix("0x")
                .is_some_and(|hex| hex.bytes().all(|b| b.is_ascii_hexdigit())))
}

/// The longest client name that is printed.
const CLIENT_NAME_MAX: usize = 64;

/// What a page's `client` parameter says about who asked. The whole address is
/// attacker-controlled, so the name is printed only when it cannot move the
/// cursor or start a line, and only as a claim.
pub(super) fn client_claim(value: &str) -> String {
    let parsed = serde_json::from_str::<serde_json::Value>(value).ok();
    let name = parsed
        .as_ref()
        .and_then(|client| client.get("name")?.as_str());
    match name.filter(|name| !name.is_empty()) {
        None => "a client it does not name".into(),
        Some(name)
            if name.chars().count() > CLIENT_NAME_MAX || !name.chars().all(super::printable) =>
        {
            format!(
                "a client name conbus will not print: not printable, or over {CLIENT_NAME_MAX} characters"
            )
        }
        Some(name) => {
            format!("asked by \"{name}\"; that is the URL's claim, and nothing checked it")
        }
    }
}

/// `www.cursor.com` under `cursor.com`, but not `notcursor.com` and not
/// `.cursor.com`: the boundary is a dot with a label in front of it.
fn subdomain(host: &str, page: &Page) -> bool {
    host.strip_suffix(page.host)
        .and_then(|front| front.strip_suffix('.'))
        .is_some_and(|front| !front.is_empty())
}

/// The parts of an address that are looked at.
pub(super) struct Address<'a> {
    scheme: String,
    host: String,
    query: &'a str,
    fragment: &'a str,
}

impl<'a> Address<'a> {
    /// `None` for anything that is not plainly `scheme://host…`.
    pub fn parse(text: &'a str) -> Option<Self> {
        let (scheme, rest) = text.split_once("://")?;
        if !valid_scheme(scheme) {
            return None;
        }
        let (host, _port, tail) = authority(rest)?;
        let (tail, fragment) = tail.split_once('#').unwrap_or((tail, ""));
        let query = tail.split_once('?').map_or("", |(_, query)| query);
        Some(Address {
            scheme: scheme.to_ascii_lowercase(),
            host,
            query,
            fragment,
        })
    }

    pub fn page(&self) -> Option<&'static Page> {
        PAGES.iter().find(|page| page.serves(self))
    }

    /// Each distinct place the page says it calls back to. A parameter named like a
    /// callback is read whatever its spelling; any other that names this machine, or
    /// a fragment that does, may be where the page really goes, so it counts as
    /// unread. That is looked for in the raw text: other values stay undecoded.
    pub fn callbacks(&self) -> Vec<Callback> {
        let mut found: Vec<Callback> = Vec::new();
        let mut add = |callback: Callback| {
            if !found.contains(&callback) {
                found.push(callback);
            }
        };
        for pair in self.query.split('&').filter(|pair| !pair.is_empty()) {
            let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
            let name = percent_decode(name);
            if names_a_callback(&name) {
                add(callback(&percent_decode(value)));
            } else if name != "client" && mentions_this_machine(value) {
                add(Callback::Unread);
            }
        }
        if mentions_this_machine(self.fragment) {
            add(Callback::Unread);
        }
        found
    }

    /// The decoded values of the query parameters in `wanted`, in order. Every
    /// other value, a token's included, stays undecoded and unkept.
    pub fn values(&self, wanted: &[&str]) -> Vec<String> {
        self.query
            .split('&')
            .filter_map(|pair| {
                let (name, value) = pair.split_once('=')?;
                wanted
                    .contains(&percent_decode(name).as_str())
                    .then(|| percent_decode(value))
            })
            .collect()
    }
}

fn valid_scheme(scheme: &str) -> bool {
    let mut chars = scheme.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

/// What follows `scheme://`: the lower-cased host, the port, and the rest.
/// Userinfo is not the host. A backslash ends the authority because a browser
/// reads it as a slash, so `https://evil.example\@auth.openai.com/` is
/// `evil.example` there and here.
fn authority(text: &str) -> Option<(String, Option<u16>, &str)> {
    let end = text.find(['/', '?', '#', '\\']).unwrap_or(text.len());
    let (authority, tail) = text.split_at(end);
    let host_and_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = split_host_port(host_and_port)?;
    Some((host, port, tail))
}

fn split_host_port(text: &str) -> Option<(String, Option<u16>)> {
    if let Some(rest) = text.strip_prefix('[') {
        let (host, after) = rest.split_once(']')?;
        host.parse::<std::net::Ipv6Addr>().ok()?;
        let port = match after {
            "" => None,
            _ => port_number(after.strip_prefix(':')?)?,
        };
        return Some((host.to_ascii_lowercase(), port));
    }
    let (host, port) = match text.rsplit_once(':') {
        Some((host, port)) => (host, port_number(port)?),
        None => (text, None),
    };
    let clean = !host.is_empty()
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    clean.then(|| (host.to_ascii_lowercase(), port))
}

/// `Some(None)` for the empty port a URL may carry, `None` when it is no port.
fn port_number(text: &str) -> Option<Option<u16>> {
    if text.is_empty() {
        return Some(None);
    }
    if !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse::<u16>().ok().filter(|port| *port != 0).map(Some)
}

/// As a browser reads a query: `+` is a space, and a `%` that does not start
/// two hex digits is itself. It works on bytes, so a multibyte character after
/// a `%` is only a character.
fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%')
            .then(|| {
                Some((
                    hex_digit(*bytes.get(i + 1)?)?,
                    hex_digit(*bytes.get(i + 2)?)?,
                ))
            })
            .flatten();
        match (hex, bytes[i]) {
            (Some((high, low)), _) => {
                out.push(high * 16 + low);
                i += 3;
            }
            (None, b'+') => {
                out.push(b' ');
                i += 1;
            }
            (None, byte) => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    char::from(byte)
        .to_digit(16)
        .and_then(|digit| u8::try_from(digit).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_query_value_decodes_as_a_browser_decodes_it() {
        for (raw, decoded) in [
            ("a%20b", "a b"),
            ("a+b", "a b"),
            ("%ED%95%9C", "\u{d55c}"),
            ("%", "%"),
            ("%4", "%4"),
            ("%zz", "%zz"),
            ("%a\u{e9}", "%a\u{e9}"),
            ("%FF", "\u{fffd}"),
        ] {
            assert_eq!(percent_decode(raw), decoded, "{raw}");
        }
    }

    #[test]
    fn the_host_is_the_authority_after_userinfo_and_before_the_port() {
        for (text, host, port) in [
            ("https://Example.COM:8443/x", "example.com", Some(8443)),
            ("https://user:1234@example.com/", "example.com", None),
            ("https://example.com:/", "example.com", None),
            ("https://[::1]:55407/", "::1", Some(55407)),
            ("https://[::1]/", "::1", None),
        ] {
            let (_, rest) = text.split_once("://").expect("scheme");
            let (parsed_host, parsed_port, _) = authority(rest).expect(text);
            assert_eq!(parsed_host, host, "{text}");
            assert_eq!(parsed_port, port, "{text}");
        }
    }

    #[test]
    fn an_authority_that_is_not_clean_does_not_parse() {
        for text in [
            "https://",
            "https://:80/",
            "https://exa mple.com/",
            "https://exa\tmple.com/",
            "https://example.com:port/",
            "https://example.com:0/",
            "https://example.com:+80/",
            "https://example.com:99999/",
            "https://[not-ipv6]:1/",
            "https://[::1]1/",
            "https://ex%61mple.com/",
            "https://ex\u{e4}mple.com/",
            "1ttp://example.com/",
            "ht tp://example.com/",
        ] {
            assert!(Address::parse(text).is_none(), "{text:?}");
        }
    }

    #[test]
    fn a_callback_is_local_only_when_its_host_is_loopback() {
        for (value, want) in [
            ("ws://127.0.0.1:1234/x", Callback::Local(1234)),
            ("ws://127.9.9.9:1234/x", Callback::Local(1234)),
            ("http://LOCALHOST:1234", Callback::Local(1234)),
            ("HTTP://localhost:1234", Callback::Local(1234)),
            ("http://[::1]:1234/", Callback::Local(1234)),
            ("http://[::ffff:127.0.0.1]:1/", Callback::Local(1)),
            ("localhost:1234/cb", Callback::Local(1234)),
            ("127.0.0.1:1234", Callback::Local(1234)),
            ("http://u:9@localhost:1234/", Callback::Local(1234)),
            ("http://localhost/cb", Callback::LocalNoPort),
            ("https://example.com:1234/", Callback::Remote),
            ("http://u:9@example.com/", Callback::Remote),
            ("http://127.0.0.1.example.com:1/", Callback::Remote),
            ("http://localhost.example.com:1/", Callback::Remote),
            ("http://10.0.0.1:1/", Callback::Remote),
            ("http://[2001:db8::1]:1/", Callback::Remote),
            ("example.com:1234/cb", Callback::Remote),
            (
                "cursor://anysphere.cursor-retrieval/oauth",
                Callback::AppScheme,
            ),
            ("VSCODE://auth", Callback::AppScheme),
            ("chrome-extension://abc/x", Callback::AppScheme),
            ("http://0.0.0.0:1/", Callback::Unread),
            ("http://[::]:1/", Callback::Unread),
            ("http://127.1:1/", Callback::Unread),
            ("http://2130706433:1/", Callback::Unread),
            ("http://0x7f.1:1/", Callback::Unread),
            ("127.1:1", Callback::Unread),
            ("someFunction", Callback::Unread),
            ("localhost/cb", Callback::Unread),
            ("/cb", Callback::Unread),
            ("", Callback::Unread),
            ("urn:ietf:wg:oauth:2.0:oob", Callback::Unread),
            ("1ttp://localhost:1/", Callback::Unread),
            ("http://localhost:port/", Callback::Unread),
            ("http://localhost:99999/", Callback::Unread),
            ("localhost:99999", Callback::Unread),
            ("http%3A%2F%2Flocalhost%3A1", Callback::Unread),
        ] {
            assert_eq!(callback(value), want, "{value}");
        }
    }

    #[test]
    fn a_name_counts_as_a_callback_whatever_its_spelling() {
        for name in [
            "redirect_uri",
            "redirectUri",
            "REDIRECT-URI",
            "post_logout_redirect_uri",
            "mcpRelayUrl",
            "relay_url",
            "callback",
            "callbackUrl",
            "return_to",
            "returnUrl",
        ] {
            assert!(names_a_callback(name), "{name}");
        }
        for name in ["scope", "state", "client_id", "token", "code", "next", ""] {
            assert!(!names_a_callback(name), "{name}");
        }
    }

    #[test]
    fn only_a_value_naming_this_machine_stands_in_the_way() {
        for raw in [
            "http%3A%2F%2FLOCALHOST%3A1",
            "http://127.0.0.1:2/",
            "ws://[::1]:2/",
            "ws%3A%2F%2F%5B%3A%3A1%5D%3A2%2F",
        ] {
            assert!(mentions_this_machine(raw), "{raw}");
        }
        for raw in [
            "https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform",
            "abc123",
            "",
        ] {
            assert!(!mentions_this_machine(raw), "{raw}");
        }
    }

    #[test]
    fn a_client_parameter_is_not_read_as_a_callback() {
        let address =
            Address::parse("https://x.example/?client=%7B%22name%22%3A%22localhost%22%7D")
                .expect("parse");
        assert!(address.callbacks().is_empty());
    }

    #[test]
    fn the_same_callback_twice_is_one_callback() {
        let address = Address::parse(
            "https://x.example/?redirect_uri=http://127.0.0.1:1/a&callback=http://localhost:1/b&redirect_url=http://127.0.0.1:2/",
        )
        .expect("parse");
        assert_eq!(
            address.callbacks(),
            [Callback::Local(1), Callback::Local(2)]
        );
    }

    #[test]
    fn only_the_values_asked_for_come_back() {
        let address =
            Address::parse("https://x.example/p?token=SECRET&client=a%20b&client=c#frag=z")
                .expect("parse");
        assert_eq!(address.values(&["client"]), ["a b", "c"]);
        assert!(address.values(&["frag"]).is_empty());
        assert!(address.values(&["absent"]).is_empty());
    }
}
