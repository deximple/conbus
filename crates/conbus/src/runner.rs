//! `conbus run` — the same TTY stays the window while the harness changes.
//!
//! The vendor TUI is still what the person sees. This process is the parent:
//! it starts the current leg, and when `hop --apply` writes a takeover it
//! stops that child and execs the next one. No chat UI. No inject into a
//! thread the user opened — Codex is resumed only by a `warm:` id we minted.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use conbus_core::event::Harness;
use conbus_core::paths::Paths;

use crate::appserver;
use crate::takeover::{self, Taken, Takeover};

/// Which binary answers for a leg.
///
/// Production resolves from the environment, exactly as it always has. Tests
/// pass explicit paths instead, because the environment is process-global and
/// `cargo test` runs tests in threads: a test that pointed `CONBUS_CLAUDE_BIN`
/// at its own stub could be answered by another test's stub, and was — this
/// module and `warm` each held a lock of their own over the same variable.
pub enum Bins {
    /// Read the environment, one resolver per leg, the way `conbus run` does.
    Env,
    /// Explicit answers, keyed by harness. Anything unnamed resolves to a
    /// binary that fails rather than to a real vendor CLI.
    #[cfg(test)]
    Fixed(Vec<(String, std::ffi::OsString)>),
}

impl Bins {
    fn get(&self, harness: &str) -> std::ffi::OsString {
        match self {
            Bins::Env => match harness {
                Harness::CODEX => appserver::codex_binary(),
                Harness::CLAUDE_CODE => claude_binary(),
                Harness::CURSOR => crate::landing::cursor_app_cli().into_os_string(),
                other => leg_binary(other),
            },
            #[cfg(test)]
            Bins::Fixed(pairs) => pairs
                .iter()
                .find(|(named, _)| named == harness)
                .map(|(_, binary)| binary.clone())
                .unwrap_or_else(|| "/nonexistent/conbus-test-leg".into()),
        }
    }
}

pub fn run(paths: &Paths, harness: &str, profile: Option<&str>) -> io::Result<()> {
    run_with(
        paths,
        harness,
        profile.map(profile_dir).transpose()?,
        &Bins::Env,
    )
}

/// `$HOME/.claude-NAME`, checked: the directory only exists once the vendor's
/// own login has created it, and conbus never creates it for them.
fn profile_dir(name: &str) -> io::Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::other("no HOME to resolve a profile under"))?;
    let dir = crate::profile::dir(&home, name);
    if !crate::profile::valid(name) || !dir.is_dir() {
        return Err(io::Error::other(format!(
            "no profile {name:?} at {} — {}",
            dir.display(),
            crate::profile::login_hint(&home, name)
        )));
    }
    Ok(dir)
}

fn run_with(paths: &Paths, harness: &str, profile: Option<PathBuf>, bins: &Bins) -> io::Result<()> {
    let requested = normalize(harness)?;
    if profile.is_some() && requested != Harness::CLAUDE_CODE {
        return Err(io::Error::other(
            "--profile is a Claude Code thing; no other harness has profiles",
        ));
    }
    let mut pending = match takeover::take(paths)? {
        Taken::Fresh { takeover, age } => {
            // Said out loud: the person asked for one leg and is about to get
            // another, in whatever checkout that hop was made from.
            println!(
                "conbus run: picking up a hop written {} ago → {}{}{}",
                conbus_core::clock::ago(age),
                takeover.next,
                warm_suffix(&takeover),
                if takeover.next == requested {
                    String::new()
                } else {
                    format!(" (instead of {requested})")
                }
            );
            Some(takeover)
        }
        Taken::Stale { next, age } => {
            println!("conbus run: {}; starting {requested}", ignored(&next, age));
            None
        }
        Taken::Nothing => None,
    };
    let mut current = match pending.as_ref() {
        Some(next) => normalize(&next.next)?,
        None => requested,
    };
    loop {
        spawn(paths, &current, pending.as_ref(), bins, profile.as_deref())?;
        match takeover::take(paths)? {
            Taken::Fresh { takeover, .. } => {
                println!(
                    "conbus run: hop → {}{}",
                    takeover.next,
                    warm_suffix(&takeover)
                );
                current = normalize(&takeover.next)?;
                pending = Some(takeover);
            }
            Taken::Stale { next, age } => {
                println!("conbus run: {}", ignored(&next, age));
                return Ok(());
            }
            Taken::Nothing => return Ok(()),
        }
    }
}

fn warm_suffix(takeover: &Takeover) -> String {
    takeover
        .thread
        .as_deref()
        .map(|id| format!(" · warm {id}"))
        .unwrap_or_default()
}

fn ignored(next: &str, age: Option<i64>) -> String {
    let when = match age {
        Some(age) => format!("written {} ago", conbus_core::clock::ago(age)),
        None => "that does not say when it was written".into(),
    };
    format!(
        "ignored a hop to {next} {when} — older than {}m, it was meant for a run that is gone",
        takeover::FRESH_FOR_SECS / 60
    )
}

fn normalize(name: &str) -> io::Result<String> {
    conbus_core::attach::resolve(name)
        .map(str::to_string)
        .ok_or_else(|| {
            let catalog = conbus_core::attach::slugs().collect::<Vec<_>>().join(", ");
            io::Error::other(format!(
                "conbus run starts a catalog leg ({catalog}), not {name:?}"
            ))
        })
}

fn spawn(
    paths: &Paths,
    harness: &str,
    takeover: Option<&Takeover>,
    bins: &Bins,
    profile: Option<&Path>,
) -> io::Result<()> {
    let mut child = vendor(harness, takeover, bins, profile)?;
    loop {
        if let Some(status) = child.try_wait()? {
            return if status.success() {
                Ok(())
            } else {
                Err(io::Error::other(format!("{harness} exited {status}")))
            };
        }
        if takeover::peek(paths)?.is_some_and(|next| next.next != harness) {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
}

fn vendor(
    harness: &str,
    takeover: Option<&Takeover>,
    bins: &Bins,
    profile: Option<&Path>,
) -> io::Result<std::process::Child> {
    let mut cmd = Command::new(bins.get(harness));
    match harness {
        Harness::CODEX => {
            if let Some(thread) = takeover.and_then(|t| t.thread.as_deref()) {
                cmd.args(["exec", "resume", thread, "continue"]);
            }
        }
        Harness::CLAUDE_CODE => {
            // The profile is the run's, not the takeover's: hop out and back
            // and the Claude that answers is still this account's.
            if let Some(dir) = profile {
                cmd.env("CLAUDE_CONFIG_DIR", dir);
            }
            if let Some(thread) = takeover.and_then(|t| t.thread.as_deref()) {
                if profile.is_none() {
                    cmd.args(["--resume", thread]);
                } else {
                    // That session was written under the default profile's
                    // directory; a profiled claude cannot see it. The fresh
                    // session is briefed from the checkout by the hooks.
                    println!(
                        "conbus run: not resuming {thread} — it lives in the \
                         default profile; this session starts fresh and the \
                         hooks brief it from the checkout"
                    );
                }
            }
        }
        Harness::CURSOR => {
            cmd.arg("agent");
            if let Some(cwd) = takeover.and_then(|t| t.cwd.as_deref()) {
                cmd.args(["--workspace", cwd, "--trust"]);
            }
        }
        _ => {}
    }
    if let Some(cwd) = takeover.and_then(|t| t.cwd.as_deref())
        && harness != Harness::CURSOR
    {
        cmd.current_dir(cwd);
    }
    cmd.stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .env("PATH", appserver::path_for_vendor())
        .spawn()
}

fn claude_binary() -> std::ffi::OsString {
    env_binary("CONBUS_CLAUDE_BIN", "claude")
}

fn leg_binary(harness: &str) -> std::ffi::OsString {
    match harness {
        Harness::CURSOR => env_binary("CONBUS_CURSOR_BIN", "agent"),
        Harness::AGY => env_binary("CONBUS_AGY_BIN", "agy"),
        Harness::OPENCODE => env_binary("CONBUS_OPENCODE_BIN", "opencode"),
        Harness::GROK => env_binary("CONBUS_GROK_BIN", "grok"),
        _ => env_binary("CONBUS_CLAUDE_BIN", "claude"),
    }
}

fn env_binary(key: &str, default: &str) -> std::ffi::OsString {
    std::env::var_os(key)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::takeover::Takeover;
    use conbus_core::paths::Paths;
    use std::fs;

    fn bins(pairs: &[(&str, &std::path::Path)]) -> Bins {
        Bins::Fixed(
            pairs
                .iter()
                .map(|(harness, path)| ((*harness).to_string(), path.as_os_str().to_owned()))
                .collect(),
        )
    }

    fn machine() -> (tempfile::TempDir, Paths) {
        let root = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::from_env(root.path().join("conbus").to_str(), None, None).expect("paths");
        fs::create_dir_all(paths.root()).expect("root");
        (root, paths)
    }

    fn stub(dir: &std::path::Path, name: &str, body: &str) -> std::path::PathBuf {
        let path = dir.join(name);
        crate::test_support::write_executable(&path, &format!("#!/bin/sh\n{body}\n"));
        path
    }

    #[test]
    fn a_takeover_replaces_the_child_in_the_same_process() {
        let (root, paths) = machine();
        let claude = stub(root.path(), "claude", "sleep 30");
        let marker = root.path().join("codex-started");
        let codex = stub(
            root.path(),
            "codex",
            &format!("echo \"$1 $2 $3\" > {}", marker.display()),
        );
        takeover::write(
            &paths,
            &Takeover {
                next: "codex".into(),
                thread: Some("thr_bus_owned".into()),
                bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
                cwd: None,
            },
        )
        .expect("takeover");

        run_with(
            &paths,
            "claude_code",
            None,
            &bins(&[("claude_code", &claude), ("codex", &codex)]),
        )
        .expect("run");

        let text = fs::read_to_string(&marker).expect("codex ran");
        assert!(
            text.contains("exec resume thr_bus_owned"),
            "the same process must start the warm thread, got {text:?}"
        );
    }

    #[test]
    fn a_takeover_nobody_took_is_ignored_and_the_requested_leg_starts() {
        // The shape left on a real machine by hops nobody ran `conbus run`
        // for: no `written_at`, pointing at another project's Codex thread.
        // Following it would resume a stranger's conversation in a stranger's
        // checkout; the person asked for Claude.
        let (root, paths) = machine();
        let claude_marker = root.path().join("claude-started");
        let codex_marker = root.path().join("codex-started");
        let claude = stub(
            root.path(),
            "claude",
            &format!("echo started > {}", claude_marker.display()),
        );
        let codex = stub(
            root.path(),
            "codex",
            &format!("echo started > {}", codex_marker.display()),
        );
        fs::write(
            paths.takeover(),
            r#"{"next":"codex","thread":"thr_old","bus_id":"01a081ef-889d-76d0-9e03-2352684e8e0c","cwd":"/elsewhere"}"#,
        )
        .expect("legacy takeover");

        run_with(
            &paths,
            "claude_code",
            None,
            &bins(&[("claude_code", &claude), ("codex", &codex)]),
        )
        .expect("run");

        assert!(claude_marker.exists(), "the requested leg must start");
        assert!(!codex_marker.exists(), "a stale hop must not be followed");
        assert!(
            !paths.takeover().exists(),
            "and it must not wait for the next run"
        );
    }

    #[test]
    fn catalog_legs_are_runnable() {
        assert_eq!(normalize("cursor").expect("ok"), Harness::CURSOR);
        assert_eq!(normalize("antigravity").expect("ok"), Harness::AGY);
        assert!(normalize("not_a_harness").is_err());
    }

    #[test]
    fn a_claude_takeover_resumes_the_bus_owned_session() {
        let (root, paths) = machine();
        let marker = root.path().join("claude-started");
        let claude = stub(
            root.path(),
            "claude",
            &format!("echo \"$1 $2\" > {}", marker.display()),
        );
        takeover::write(
            &paths,
            &Takeover {
                next: "claude_code".into(),
                thread: Some("sess_bus_owned".into()),
                bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
                cwd: None,
            },
        )
        .expect("takeover");

        run_with(&paths, "codex", None, &bins(&[("claude_code", &claude)])).expect("run");

        let text = fs::read_to_string(&marker).expect("claude ran");
        assert!(
            text.contains("--resume sess_bus_owned"),
            "hop-back must resume the bus-owned session, got {text:?}"
        );
    }

    #[test]
    fn two_runs_with_different_binaries_do_not_answer_each_other() {
        // The defect this seam exists for. Both of these used to point the
        // process-global `CONBUS_CLAUDE_BIN` at their own stub under a lock
        // this module kept to itself, while `warm` did the same under a lock
        // of its own — so running the two modules together failed five times
        // in eight, on a different test each run. Nothing here touches the
        // environment, so the answer cannot travel between threads.
        let started: Vec<_> = ["alpha", "beta"]
            .into_iter()
            .map(|name| {
                std::thread::spawn(move || {
                    let (root, paths) = machine();
                    let marker = root.path().join(format!("{name}-started"));
                    let claude = stub(
                        root.path(),
                        "claude",
                        &format!("echo \"{name} $1 $2\" > {}", marker.display()),
                    );
                    takeover::write(
                        &paths,
                        &Takeover {
                            next: "claude_code".into(),
                            thread: Some(format!("sess_{name}")),
                            bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
                            cwd: None,
                        },
                    )
                    .expect("takeover");

                    run_with(&paths, "codex", None, &bins(&[("claude_code", &claude)]))
                        .expect("run");

                    fs::read_to_string(&marker).expect("claude ran")
                })
            })
            .collect();

        let seen: Vec<String> = started
            .into_iter()
            .map(|handle| handle.join().expect("thread"))
            .collect();

        assert!(
            seen[0].contains("alpha --resume sess_alpha"),
            "the first run was answered by the wrong binary: {seen:?}"
        );
        assert!(
            seen[1].contains("beta --resume sess_beta"),
            "the second run was answered by the wrong binary: {seen:?}"
        );
    }

    #[test]
    fn a_profiled_run_hands_the_vendor_its_config_dir() {
        // `conbus run claude_code --profile work` runs `claude` with
        // CLAUDE_CONFIG_DIR pointing at the profile — the vendor's own
        // documented side-by-side mechanism, so its hooks and statusLine
        // inherit it too.
        let (root, paths) = machine();
        let profile = root.path().join(".claude-work");
        fs::create_dir_all(&profile).expect("profile dir");
        let marker = root.path().join("claude-env");
        let claude = stub(
            root.path(),
            "claude",
            &format!(
                "echo \"${{CLAUDE_CONFIG_DIR:-unset}}\" > {}",
                marker.display()
            ),
        );

        run_with(
            &paths,
            "claude_code",
            Some(profile.clone()),
            &bins(&[("claude_code", &claude)]),
        )
        .expect("run");

        let text = fs::read_to_string(&marker).expect("claude ran");
        assert_eq!(text.trim(), profile.display().to_string());
    }

    #[test]
    fn a_profiled_run_does_not_resume_the_default_profile_session() {
        // A takeover can carry `warm-claude:<session>` — a session that lives
        // in the default profile's directory, which a profiled claude cannot
        // see. Resuming it would fail or, worse, mix accounts: the profiled
        // leg starts fresh and the hooks brief it from the checkout.
        let (root, paths) = machine();
        let profile = root.path().join(".claude-work");
        fs::create_dir_all(&profile).expect("profile dir");
        let marker = root.path().join("claude-args");
        let claude = stub(
            root.path(),
            "claude",
            &format!("echo \"$*\" > {}", marker.display()),
        );
        takeover::write(
            &paths,
            &Takeover {
                next: "claude_code".into(),
                thread: Some("sess_default_profile".into()),
                bus_id: "01a072d2-2e0f-7b72-8afb-b4045e646cde".into(),
                cwd: None,
            },
        )
        .expect("takeover");

        run_with(
            &paths,
            "claude_code",
            Some(profile),
            &bins(&[("claude_code", &claude)]),
        )
        .expect("run");

        let text = fs::read_to_string(&marker).expect("claude ran");
        assert!(
            !text.contains("--resume"),
            "the default profile's session must not be resumed: {text:?}"
        );
    }

    #[test]
    fn a_profiled_run_only_starts_claude() {
        // `--profile` on any other leg is refused: no other vendor has a
        // side-by-side account directory for it to mean.
        let (root, paths) = machine();
        let profile = root.path().join(".claude-work");
        fs::create_dir_all(&profile).expect("profile dir");
        let err = run_with(&paths, "codex", Some(profile), &bins(&[]))
            .expect_err("non-claude leg refuses a profile");
        assert!(err.to_string().contains("Claude Code"), "{err}");
    }

    #[test]
    fn a_profile_stays_the_runs_across_a_hop_back() {
        // claude → codex → claude inside one `conbus run --profile`: each
        // claude leg answers the same account.
        let (root, paths) = machine();
        let profile = root.path().join(".claude-work");
        fs::create_dir_all(&profile).expect("profile dir");
        let env_marker = root.path().join("claude-env");
        let sent = root.path().join("hopped-out");
        let now = conbus_core::clock::Timestamp::now();
        let takeover = |next: &str| {
            format!(
                "printf '%s\\n' '{{\"next\":\"{next}\",\"bus_id\":\"01a072d2-2e0f-7b72-8afb-b4045e646cde\",\"written_at\":\"{now}\"}}' > {}",
                paths.takeover().display(),
            )
        };
        let claude = stub(
            root.path(),
            "claude",
            &format!(
                "echo \"${{CLAUDE_CONFIG_DIR:-unset}}\" >> {}\n[ -f {} ] || {{ touch {}; {}; }}",
                env_marker.display(),
                sent.display(),
                sent.display(),
                takeover("codex"),
            ),
        );
        let codex = stub(root.path(), "codex", &takeover("claude_code"));

        run_with(
            &paths,
            "claude_code",
            Some(profile.clone()),
            &bins(&[("claude_code", &claude), ("codex", &codex)]),
        )
        .expect("run");

        let text = fs::read_to_string(&env_marker).expect("claude ran");
        assert_eq!(
            text.lines().collect::<Vec<_>>(),
            vec![profile.display().to_string(); 2],
            "hop out and back must keep the profile env on both claude legs"
        );
    }
}
