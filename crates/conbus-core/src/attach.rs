//! What each harness can actually do.
//!
//! The router does not assume a missing ability. Automatic failover needs a
//! quota signal on the source and a usable overflow target. A directed hop
//! may name any catalog leg; that is a handshake, not a fabricated signal.

use crate::event::Harness;

/// One attachable leg.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Caps {
    pub slug: &'static str,
    pub aliases: &'static [&'static str],
    /// Official machine-readable remaining-quota channel we have captured.
    /// `/usage` in a TUI is not this. Claude's StatusLine stdin `rate_limits`
    /// is: the unmodified binary already fetched it. We do not call
    /// `oauth/usage` ourselves.
    pub quota_signal: bool,
    /// History write that is not a model turn.
    pub history_inject_no_model: bool,
    /// Auto-overflow may land here. Directed hop may still name a false.
    pub usable_as_target: bool,
}

pub const CATALOG: &[Caps] = &[
    Caps {
        slug: Harness::CLAUDE_CODE,
        aliases: &["claude"],
        quota_signal: true,
        history_inject_no_model: true,
        usable_as_target: true,
    },
    Caps {
        slug: Harness::CODEX,
        aliases: &[],
        quota_signal: true,
        history_inject_no_model: true,
        usable_as_target: true,
    },
    Caps {
        slug: Harness::CURSOR,
        aliases: &[],
        quota_signal: false,
        history_inject_no_model: false,
        usable_as_target: false,
    },
    Caps {
        slug: Harness::AGY,
        aliases: &["antigravity"],
        quota_signal: false,
        history_inject_no_model: false,
        usable_as_target: false,
    },
    Caps {
        slug: Harness::OPENCODE,
        aliases: &[],
        quota_signal: false,
        history_inject_no_model: false,
        usable_as_target: false,
    },
    Caps {
        slug: Harness::GROK,
        aliases: &[],
        quota_signal: false,
        history_inject_no_model: false,
        usable_as_target: false,
    },
];

pub fn resolve(name: &str) -> Option<&'static str> {
    let name = name.trim();
    CATALOG.iter().find_map(|caps| {
        (caps.slug == name || caps.aliases.contains(&name)).then_some(caps.slug)
    })
}

pub fn caps(slug: &str) -> Option<&'static Caps> {
    let slug = resolve(slug)?;
    CATALOG.iter().find(|caps| caps.slug == slug)
}

/// Where auto overflow goes. Dual-warm pair first; everyone else to Codex.
pub fn overflow(slug: &str) -> Option<&'static str> {
    let slug = resolve(slug)?;
    match slug {
        Harness::CLAUDE_CODE => Some(Harness::CODEX),
        Harness::CODEX => Some(Harness::CLAUDE_CODE),
        _ => Some(Harness::CODEX),
    }
}

pub fn slugs() -> impl Iterator<Item = &'static str> {
    CATALOG.iter().map(|caps| caps.slug)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn aliases_resolve_and_unknown_names_do_not() {
        assert_eq!(resolve("claude"), Some(Harness::CLAUDE_CODE));
        assert_eq!(resolve("antigravity"), Some(Harness::AGY));
        assert_eq!(resolve("cursor"), Some(Harness::CURSOR));
        assert_eq!(resolve("not_a_harness"), None);
    }

    #[test]
    fn agy_is_not_an_auto_overflow_target() {
        assert!(!caps(Harness::AGY).expect("agy").usable_as_target);
        assert_eq!(overflow(Harness::AGY), Some(Harness::CODEX));
        assert_eq!(overflow(Harness::CURSOR), Some(Harness::CODEX));
    }

    #[test]
    fn only_the_dual_warm_pair_has_both_route_abilities() {
        for slug in [Harness::CLAUDE_CODE, Harness::CODEX] {
            let caps = caps(slug).expect("pair");
            assert!(caps.quota_signal && caps.history_inject_no_model && caps.usable_as_target);
        }
        for slug in [Harness::CURSOR, Harness::AGY, Harness::OPENCODE, Harness::GROK] {
            let caps = caps(slug).expect("attach");
            assert!(!caps.quota_signal);
            assert!(!caps.usable_as_target);
        }
    }
}
