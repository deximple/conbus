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
    pub label: &'static str,
}

pub(super) const UNKNOWN_PAGE: &str = "unknown page";

/// The scheme of the one page that is not a website.
pub(super) const BRIDGE_SCHEME: &str = "chrome-extension";

/// One table, for the command and for the `--html` page's script. A label says
/// what the host is, because the path is not looked at: `github.com` is where
/// `gh` signs in, and every other page of that site too.
pub(super) const PAGES: &[Page] = &[
    Page {
        scheme: BRIDGE_SCHEME,
        host: "mmlmfjhmonkocbjadbfplnigmagldckm",
        subdomains: false,
        label: "Playwright MCP Bridge connect page (a Chrome extension)",
    },
    Page {
        scheme: "https",
        host: "accounts.google.com",
        subdomains: false,
        label: "Google sign-in (agy and other Google-backed tools)",
    },
    Page {
        scheme: "https",
        host: "auth.openai.com",
        subdomains: false,
        label: "OpenAI sign-in (Codex)",
    },
    Page {
        scheme: "https",
        host: "claude.ai",
        subdomains: true,
        label: "a claude.ai page (Claude Code signs in here)",
    },
    Page {
        scheme: "https",
        host: "console.anthropic.com",
        subdomains: false,
        label: "Anthropic Console (Claude Code signs in here)",
    },
    Page {
        scheme: "https",
        host: "cursor.com",
        subdomains: true,
        label: "a cursor.com page (Cursor signs in here)",
    },
    Page {
        scheme: "https",
        host: "x.ai",
        subdomains: true,
        label: "an x.ai page (Grok signs in here)",
    },
    Page {
        scheme: "https",
        host: "github.com",
        subdomains: false,
        label: "a github.com page (gh signs in here)",
    },
];

impl Page {
    fn serves(&self, address: &Address<'_>) -> bool {
        address.scheme == self.scheme
            && (address.host == self.host || (self.subdomains && subdomain(&address.host, self)))
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
}

impl<'a> Address<'a> {
    /// `None` for anything that is not plainly `scheme://host…`.
    pub fn parse(text: &'a str) -> Option<Self> {
        let (scheme, rest) = text.split_once("://")?;
        if !valid_scheme(scheme) {
            return None;
        }
        let (host, _port, tail) = authority(rest)?;
        let tail = tail.split_once('#').map_or(tail, |(before, _)| before);
        let query = tail.split_once('?').map_or("", |(_, query)| query);
        Some(Address {
            scheme: scheme.to_ascii_lowercase(),
            host,
            query,
        })
    }

    pub fn page(&self) -> Option<&'static Page> {
        PAGES.iter().find(|page| page.serves(self))
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
    fn only_the_values_asked_for_come_back() {
        let address =
            Address::parse("https://x.example/p?token=SECRET&client=a%20b&client=c#frag=z")
                .expect("parse");
        assert_eq!(address.values(&["client"]), ["a b", "c"]);
        assert!(address.values(&["frag"]).is_empty());
        assert!(address.values(&["absent"]).is_empty());
    }
}
