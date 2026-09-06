//! Where Conbus keeps its data.

use std::path::{Path, PathBuf};

/// The resolved data location, and how we got there.
///
/// `doctor` reports this verbatim: a home-relative path that resolves
/// differently under Windows and WSL is a known way to lose a log.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Paths {
    root: PathBuf,
    origin: Origin,
}

/// Which rule produced the root.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Origin {
    /// `$CONBUS_HOME`, set explicitly.
    Explicit,
    /// `$XDG_STATE_HOME/conbus`.
    XdgState,
    /// `$HOME/.conbus`.
    Home,
}

/// An exported-but-empty variable is unset, not a path to the filesystem root.
fn set(value: Option<&str>) -> Option<&str> {
    value.filter(|text| !text.is_empty())
}

/// Neither an override nor a home directory was available.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NoHomeError;

impl Paths {
    /// Resolve from an explicit environment, so the rules can be tested without
    /// mutating the process environment.
    pub fn from_env(
        conbus_home: Option<&str>,
        xdg_state_home: Option<&str>,
        home: Option<&str>,
    ) -> Result<Self, NoHomeError> {
        if let Some(explicit) = set(conbus_home) {
            return Ok(Paths {
                root: PathBuf::from(explicit),
                origin: Origin::Explicit,
            });
        }
        if let Some(state) = set(xdg_state_home) {
            return Ok(Paths {
                root: Path::new(state).join("conbus"),
                origin: Origin::XdgState,
            });
        }
        if let Some(home) = set(home) {
            return Ok(Paths {
                root: Path::new(home).join(".conbus"),
                origin: Origin::Home,
            });
        }
        Err(NoHomeError)
    }

    /// Resolve from the real process environment.
    pub fn resolve() -> Result<Self, NoHomeError> {
        let read = |key: &str| std::env::var(key).ok();
        Self::from_env(
            read("CONBUS_HOME").as_deref(),
            read("XDG_STATE_HOME").as_deref(),
            read("HOME").as_deref(),
        )
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn origin(&self) -> Origin {
        self.origin
    }

    /// The append-only log directory.
    pub fn log_dir(&self) -> PathBuf {
        self.root.join("log")
    }

    /// The daemon's control socket.
    pub fn socket(&self) -> PathBuf {
        self.root.join("conbus.sock")
    }

    /// Written by `hop --apply` so `conbus run` can exec the next leg
    /// in the same TTY, without the person opening a vendor window.
    pub fn takeover(&self) -> PathBuf {
        self.root.join("takeover.json")
    }

    /// Accepted facts. Derived from the log, confirmed here.
    pub fn facts(&self) -> PathBuf {
        self.root.join("facts.jsonl")
    }

    /// Workspace links: facts from `from` are visible in `to`.
    pub fn links(&self) -> PathBuf {
        self.root.join("links.jsonl")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_state_home_wins_over_the_home_default() {
        let paths = Paths::from_env(None, Some("/var/state"), Some("/home/dev")).expect("resolve");

        assert_eq!(paths.root(), Path::new("/var/state/conbus"));
        assert_eq!(paths.origin(), Origin::XdgState);
    }

    #[test]
    fn without_xdg_state_home_the_root_is_a_dotfile_under_home() {
        let paths = Paths::from_env(None, None, Some("/home/dev")).expect("resolve");

        assert_eq!(paths.root(), Path::new("/home/dev/.conbus"));
        assert_eq!(paths.origin(), Origin::Home);
    }

    #[test]
    fn an_explicit_conbus_home_overrides_everything() {
        let paths = Paths::from_env(Some("/tmp/bus"), Some("/var/state"), Some("/home/dev"))
            .expect("resolve");

        assert_eq!(paths.root(), Path::new("/tmp/bus"));
        assert_eq!(paths.origin(), Origin::Explicit);
    }

    #[test]
    fn an_empty_variable_is_treated_as_unset() {
        // A shell that exports `XDG_STATE_HOME=` must not put the log at
        // `/conbus`, which is where naive joining would land it.
        let paths = Paths::from_env(Some(""), Some(""), Some("/home/dev")).expect("resolve");

        assert_eq!(paths.root(), Path::new("/home/dev/.conbus"));
        assert_eq!(paths.origin(), Origin::Home);
    }

    #[test]
    fn with_no_home_at_all_resolution_fails_rather_than_guessing() {
        assert_eq!(Paths::from_env(None, None, None), Err(NoHomeError));
        assert_eq!(
            Paths::from_env(Some(""), Some(""), Some("")),
            Err(NoHomeError)
        );
    }

    #[test]
    fn the_log_and_socket_live_under_the_root() {
        let paths = Paths::from_env(Some("/tmp/bus"), None, None).expect("resolve");

        assert_eq!(paths.log_dir(), Path::new("/tmp/bus/log"));
        assert_eq!(paths.socket(), Path::new("/tmp/bus/conbus.sock"));
        assert_eq!(paths.takeover(), Path::new("/tmp/bus/takeover.json"));
        assert_eq!(paths.facts(), Path::new("/tmp/bus/facts.jsonl"));
        assert_eq!(paths.links(), Path::new("/tmp/bus/links.jsonl"));
    }
}
