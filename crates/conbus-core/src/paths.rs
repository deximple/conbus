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

/// The longest Unix socket path the kernel binds, in bytes, not counting the
/// terminating NUL: `sockaddr_un.sun_path` is 104 bytes on macOS and the BSDs,
/// 108 on Linux.
#[cfg(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
))]
pub const SOCKET_PATH_MAX: usize = 103;
#[cfg(not(any(
    target_os = "macos",
    target_os = "ios",
    target_os = "freebsd",
    target_os = "openbsd",
    target_os = "netbsd"
)))]
pub const SOCKET_PATH_MAX: usize = 107;

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

    /// Keep the data root to its owner: make it 0700, and close one an
    /// older build left open.
    ///
    /// The log under it holds prompts and replies, and a macOS home directory
    /// is 0755, so a root made with the default umask let every other account
    /// on the Mac read the conversations. A closed root closes everything in
    /// it, whatever the files' own modes.
    pub fn keep_private(&self) -> std::io::Result<()> {
        keep_dir_private(&self.root)
    }

    /// The append-only log directory.
    pub fn log_dir(&self) -> PathBuf {
        self.root.join("log")
    }

    /// The daemon's control socket.
    pub fn socket(&self) -> PathBuf {
        self.root.join("conbus.sock")
    }

    /// Why the socket cannot live here, when it cannot.
    ///
    /// The kernel keeps a Unix socket's path in a fixed-size field, and the
    /// standard library's refusal — "path must be shorter than SUN_LEN" — names
    /// neither the path nor the fix. A `CONBUS_HOME` under a deep temp or
    /// project directory crosses the limit easily, and the daemon then dies
    /// before it has said anything a person could act on.
    pub fn socket_problem(&self) -> Option<String> {
        let socket = self.socket();
        let bytes = socket.as_os_str().len();
        (bytes > SOCKET_PATH_MAX).then(|| {
            format!(
                "the socket path {} is {bytes} bytes and this system allows {SOCKET_PATH_MAX}; \
                 set CONBUS_HOME to a shorter directory",
                socket.display()
            )
        })
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

    /// The local PreToolUse policy: operator-written rules that tighten what
    /// a hook allows. Read by the hook process before the daemon is asked.
    pub fn policy(&self) -> PathBuf {
        self.root.join("policy.json")
    }

    /// Workspace links: facts from `from` are visible in `to`.
    pub fn links(&self) -> PathBuf {
        self.root.join("links.jsonl")
    }

    /// Last StatusLine `rate_limits` the vendor fed us, per harness.
    ///
    /// Derived, rebuildable, not a conversation record. Claude fills this
    /// when its StatusLine command runs; we never call oauth/usage ourselves.
    pub fn quota_cache(&self, harness: &str) -> PathBuf {
        self.root.join("quota").join(format!("{harness}.json"))
    }

    /// Last time the StatusLine command ran, even if `rate_limits` was absent.
    pub fn quota_seen(&self, harness: &str) -> PathBuf {
        self.root.join("quota").join(format!("{harness}.seen.json"))
    }

    /// Last hook payload field names for a harness that has no remaining-quota
    /// JSON yet. Names only — proof we looked, never conversation content.
    pub fn quota_keys(&self, harness: &str) -> PathBuf {
        self.root.join("quota").join(format!("{harness}.keys.json"))
    }

    /// Official Claude.ai / Desktop `conversations.json` (or zip) drop folder.
    /// The daemon ingests new Chat turns from here on start and again when
    /// a newer export lands — Chat-only windows still have no hook.
    pub fn chat_export(&self) -> PathBuf {
        self.root.join("chat-export")
    }
}

/// Make `dir` (and any parent it needs) 0700, and close it if it exists open.
pub fn keep_dir_private(dir: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_data_root_is_kept_to_its_owner() {
        // A root an older build made 0755 is closed when the daemon starts,
        // and one that does not exist yet is made closed.
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().expect("tempdir");
        let open = home.path().join("open");
        std::fs::create_dir(&open).expect("dir");
        std::fs::set_permissions(&open, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        let fresh = home.path().join("fresh").join("conbus");

        for root in [&open, &fresh] {
            let paths =
                Paths::from_env(Some(root.to_str().expect("utf-8")), None, None).expect("paths");
            paths.keep_private().expect("private");
            let mode = std::fs::metadata(root)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o700, "{}", root.display());
        }
    }

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
        assert_eq!(paths.policy(), Path::new("/tmp/bus/policy.json"));
        assert_eq!(paths.links(), Path::new("/tmp/bus/links.jsonl"));
        assert_eq!(
            paths.quota_cache("claude_code"),
            Path::new("/tmp/bus/quota/claude_code.json")
        );
        assert_eq!(
            paths.quota_seen("claude_code"),
            Path::new("/tmp/bus/quota/claude_code.seen.json")
        );
        assert_eq!(
            paths.quota_keys("cursor"),
            Path::new("/tmp/bus/quota/cursor.keys.json")
        );
        assert_eq!(paths.chat_export(), Path::new("/tmp/bus/chat-export"));
    }

    #[test]
    fn a_home_too_deep_for_a_socket_says_so_and_names_the_fix() {
        let short = Paths::from_env(Some("/Users/someone/.conbus"), None, None).expect("resolve");
        assert_eq!(short.socket_problem(), None);

        // `conbus.sock` plus its slash is 12 bytes; this root leaves one too many.
        let deep = "/".to_string() + &"d".repeat(SOCKET_PATH_MAX - 12);
        let paths = Paths::from_env(Some(&deep), None, None).expect("resolve");
        let problem = paths.socket_problem().expect("one byte over the limit");
        assert!(problem.contains("CONBUS_HOME"), "{problem}");
        assert!(
            problem.contains(&(SOCKET_PATH_MAX + 1).to_string()),
            "{problem}"
        );

        let exact = "/".to_string() + &"d".repeat(SOCKET_PATH_MAX - 13);
        let fits = Paths::from_env(Some(&exact), None, None).expect("resolve");
        assert_eq!(fits.socket().as_os_str().len(), SOCKET_PATH_MAX);
        assert_eq!(fits.socket_problem(), None);
    }
}
