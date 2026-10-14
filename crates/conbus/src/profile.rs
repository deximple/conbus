//! Claude Code profiles: one person's second account, switched by hand.
//!
//! A profile `NAME` is the directory `$HOME/.claude-NAME`, created and logged
//! in through the vendor's own flow (`CLAUDE_CONFIG_DIR=... claude`, then
//! `/login`). conbus never logs anyone in and never opens anything inside
//! that directory — the credentials in it are not ours to read. Switching is
//! manual only: no limit signal, route or hop moves a conversation between a
//! person's accounts, because the vendor's terms forbid circumventing a usage
//! limit.

use std::path::{Path, PathBuf};

/// Whether `NAME` may name a profile at all. The name ends up in a directory
/// name, a quota file and a quota label, so the alphabet is the whole defence.
pub fn valid(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `$HOME/.claude-NAME` — the directory the vendor's own login flow fills.
pub fn dir(home: &Path, name: &str) -> PathBuf {
    home.join(format!(".claude-{name}"))
}

/// What to tell a person whose profile directory is not there yet: the
/// profile exists only once the vendor's own login has created it.
pub fn login_hint(home: &Path, name: &str) -> String {
    format!(
        "log in first with `CLAUDE_CONFIG_DIR={} claude` and `/login`",
        dir(home, name).display()
    )
}

/// Which profile this process is a child of, when it is one.
///
/// The hook and StatusLine children inherit `CLAUDE_CONFIG_DIR` from the
/// vendor — confirmed live, because the env-var docs do not promise
/// subprocesses see it. Unset, or the default `$HOME/.claude`, is no profile;
/// anything else is named by the directory's basename with a leading
/// `.claude-` stripped, then reduced to the profile alphabet.
pub fn current() -> Option<String> {
    let dir = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from)?;
    let home = std::env::var_os("HOME").map(PathBuf::from)?;
    of(&dir, &home)
}

fn of(dir: &Path, home: &Path) -> Option<String> {
    if dir == home.join(".claude") {
        return None;
    }
    let base = dir.file_name()?.to_str()?;
    let raw = base.strip_prefix(".claude-").unwrap_or(base);
    let cleaned: String = raw
        .to_lowercase()
        .chars()
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();
    (!cleaned.is_empty()).then_some(cleaned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_profile_name_is_the_profile_alphabet() {
        assert!(valid("work"));
        assert!(valid("team-2"));
        assert!(!valid(""));
        assert!(!valid("Work"));
        assert!(!valid("a_b"));
        assert!(!valid("../etc"));
        assert!(!valid(".claude"));
    }

    #[test]
    fn the_default_config_dir_is_no_profile() {
        let home = Path::new("/home/person");
        assert_eq!(of(&home.join(".claude"), home), None);
    }

    #[test]
    fn a_profile_dir_is_named_by_its_basename() {
        let home = Path::new("/home/person");
        assert_eq!(
            of(&home.join(".claude-work"), home),
            Some("work".to_string())
        );
        assert_eq!(
            of(Path::new("/home/person/.claude-instances/work"), home),
            Some("work".to_string())
        );
        assert_eq!(
            of(&home.join(".claude-Team_2"), home),
            Some("team2".to_string())
        );
        assert_eq!(of(Path::new("/"), home), None);
    }
}
