//! How fresh the checkout is, for the pack's `code_freshness` line.
//!
//! `gh` is consulted when a hop is decided. A failure is not a routing
//! failure: the hop is recorded and the pack says the code is stale.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

use conbus_core::clock::Timestamp;

/// How long `git` or `gh` may take before the probe gives up on it.
///
/// A hop is decided while a person waits on an agent that just stopped, and
/// this probe only feeds a freshness line. On 2026-09-27 the first organic hop
/// reached its Codex thread 21.6 minutes after the decision, with `gh` and
/// `git` on that path and no limit on either.
pub const LIMIT: Duration = Duration::from_secs(5);

/// How long a child that was killed is given to go.
const KILL_GRACE: Duration = Duration::from_secs(1);

/// What the hop printed and the pack later quotes.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Freshness {
    Fresh { sha: String, note: String },
    Stale { sha: Option<String>, why: String },
}

impl Freshness {
    pub fn line(&self) -> String {
        match self {
            Freshness::Fresh { sha, note } => format!("fresh · {sha} · {note}"),
            Freshness::Stale {
                sha: Some(sha),
                why,
            } => format!("stale · {sha} · {why}"),
            Freshness::Stale { sha: None, why } => format!("stale · {why}"),
        }
    }
}

/// Look at `workspace` without failing the caller.
///
/// `CONBUS_GH` names the binary so a probe can watch `gh` fail without
/// hiding the real one; routing does not read it for any other reason.
pub fn probe(workspace: Option<&str>) -> Freshness {
    let bin = std::env::var("CONBUS_GH").unwrap_or_else(|_| "gh".into());
    probe_with(&bin, workspace)
}

/// `gh_bin` is the binary to run, so a test can point at a stub that fails.
pub fn probe_with(gh_bin: &str, workspace: Option<&str>) -> Freshness {
    let Some(dir) = workspace.filter(|d| !d.is_empty() && Path::new(d).is_dir()) else {
        return Freshness::Stale {
            sha: None,
            why: "no checkout".into(),
        };
    };
    let sha = git_sha(dir);
    match gh_note(gh_bin, dir) {
        Ok(note) => Freshness::Fresh {
            sha: sha.unwrap_or_else(|| "unknown".into()),
            note,
        },
        Err(why) => Freshness::Stale { sha, why },
    }
}

/// Commits `HEAD` in `dir` gained from `since` (a UTC day) up to `until`, by
/// commit time. None when `dir` is not a repository or `git` does not answer
/// within `limit`: a count that could not be taken is not a zero.
pub fn commits(dir: &str, since: &str, until: Option<&str>, limit: Duration) -> Option<u64> {
    let midnight = |day: &str| -> Option<i64> {
        let ts: Timestamp = format!("{day}T00:00:00.000Z").parse().ok()?;
        Some(ts.seconds_since(Timestamp::from_unix_seconds(0)?))
    };
    let mut git = folder_git(dir)?;
    git.args(["rev-list", "--count"])
        .arg(format!("--since=@{}", midnight(since)?));
    if let Some(until) = until {
        // `--until` keeps a commit made at that very second, and the window
        // the log uses does not: the second before is the window's last.
        git.arg(format!("--until=@{}", midnight(until)? - 1));
    }
    git.arg("HEAD");
    let out = output_within(git, limit).ok()?;
    if !out.status.success() {
        return None;
    }
    String::from_utf8_lossy(&out.stdout).trim().parse().ok()
}

/// The checkout `dir` belongs to: for a worktree, the main checkout it was
/// added from. None when `dir` is not in a repository `git` answers for within
/// `limit`. A folder that is gone — a worktree removed once its work landed —
/// is placed by the nearest folder above it that is still there. A submodule
/// or a bare repository has no `.git` of its own to stand beside, and is left
/// to stand for itself.
pub fn repository(dir: &str, limit: Duration) -> Option<String> {
    let there = Path::new(dir).ancestors().find(|folder| folder.is_dir())?;
    let mut git = folder_git(there.to_str()?)?;
    git.args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    let out = output_within(git, limit).ok()?;
    if !out.status.success() {
        return None;
    }
    let common = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let common = Path::new(&common);
    if common.file_name()? != ".git" {
        return None;
    }
    Some(common.parent()?.to_str()?.to_string())
}

/// `git -C dir`, for a folder an agent reported. None for a relative one: it
/// was relative to wherever the agent ran, not to here. The variables that
/// would point `git` at another repository than `dir` are not passed on.
fn folder_git(dir: &str) -> Option<Command> {
    if !Path::new(dir).is_absolute() {
        return None;
    }
    let mut git = Command::new("git");
    git.args(["-C", dir]);
    for variable in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
    ] {
        git.env_remove(variable);
    }
    Some(git)
}

fn git_sha(dir: &str) -> Option<String> {
    let mut git = Command::new("git");
    git.args(["-C", dir, "rev-parse", "--short", "HEAD"]);
    let out = output_within(git, LIMIT).ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

fn gh_note(gh_bin: &str, dir: &str) -> Result<String, String> {
    let mut gh = Command::new(gh_bin);
    gh.args(["pr", "view", "--json", "state,number"])
        .current_dir(dir);
    let out = output_within(gh, LIMIT)?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        let err = err.trim();
        if err.is_empty() {
            return Err(format!("gh exited {}", out.status));
        }
        return Err(format!("gh: {}", err.lines().next().unwrap_or(err)));
    }
    let value: serde_json::Value = serde_json::from_slice(&out.stdout)
        .map_err(|_| "gh returned unreadable json".to_string())?;
    let number = value.get("number").and_then(|n| n.as_u64());
    let state = value.get("state").and_then(|s| s.as_str()).unwrap_or("?");
    match number {
        Some(n) => Ok(format!("PR #{n} {state}")),
        None => Ok(format!("PR {state}")),
    }
}

/// Run `command` to completion and collect its output, or stop it once `limit`
/// has passed. Both outputs are small here, so reading them after exit cannot
/// fill a pipe and hold the child up.
pub(crate) fn output_within(mut command: Command, limit: Duration) -> Result<Output, String> {
    let name = command.get_program().to_string_lossy().into_owned();
    let name = Path::new(&name)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or(name);
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| format!("{name} unavailable ({err})"))?;
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                // A child stuck in the kernel — a `git` on a mount that went
                // away — outlives the kill; it is not waited on for long.
                let reaped = Instant::now() + KILL_GRACE;
                while matches!(child.try_wait(), Ok(None)) && Instant::now() < reaped {
                    std::thread::sleep(Duration::from_millis(20));
                }
                return Err(format!("{name} did not answer within {}s", limit.as_secs()));
            }
            Err(err) => return Err(format!("{name} could not be waited on ({err})")),
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut out) = child.stdout.take() {
        let _ = out.read_to_end(&mut stdout);
    }
    if let Some(mut err) = child.stderr.take() {
        let _ = err.read_to_end(&mut stderr);
    }
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A checkout that exists wherever the tests run. These used to name
    /// `/Applications/conbus`, which exists on one machine: anywhere else the
    /// probe took the "no checkout" path and never reached `gh` at all.
    const CHECKOUT: &str = env!("CARGO_MANIFEST_DIR");

    /// A repository of three commits: noon on 2026-10-01, midnight starting
    /// 2026-10-02, and noon on 2026-10-04, UTC.
    fn three_days_of_commits() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("dir");
        let git = |args: &[&str], when: Option<&str>| {
            let mut git = Command::new("git");
            git.arg("-C").arg(dir.path()).args(args);
            git.env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_INDEX_FILE");
            if let Some(when) = when {
                git.env("GIT_AUTHOR_DATE", when)
                    .env("GIT_COMMITTER_DATE", when);
            }
            assert!(git.output().expect("git").status.success(), "git {args:?}");
        };
        git(&["init", "-q"], None);
        for when in [
            "2026-10-01T12:00:00Z",
            "2026-10-02T00:00:00Z",
            "2026-10-04T12:00:00Z",
        ] {
            git(
                &[
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "commit",
                    "-q",
                    "--allow-empty",
                    "-m",
                    when,
                ],
                Some(when),
            );
        }
        dir
    }

    #[test]
    fn commits_counts_what_head_gained_inside_the_window() {
        let repo = three_days_of_commits();
        let dir = repo.path().to_str().expect("utf-8");
        assert_eq!(commits(dir, "2026-10-01", None, LIMIT), Some(3));
        assert_eq!(commits(dir, "2026-10-02", None, LIMIT), Some(2));
        // `until` is exclusive, the way the log's window is: the commit made
        // at midnight starting 2026-10-02 is that day's, not the day before's.
        assert_eq!(
            commits(dir, "2026-10-01", Some("2026-10-02"), LIMIT),
            Some(1)
        );
        assert_eq!(
            commits(dir, "2026-10-02", Some("2026-10-03"), LIMIT),
            Some(1)
        );
        assert_eq!(commits(dir, "2026-10-05", None, LIMIT), Some(0));
    }

    #[test]
    fn a_worktree_and_a_subfolder_belong_to_the_checkout_they_came_from() {
        let repo = three_days_of_commits();
        let main = repo.path().canonicalize().expect("path");
        let main_str = main.to_str().expect("utf-8");
        let worktree = main.join("wt");
        let added = Command::new("git")
            .args(["-C", main_str, "worktree", "add", "-q", "--detach"])
            .arg(&worktree)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE")
            .output()
            .expect("git");
        assert!(added.status.success(), "{added:?}");
        std::fs::create_dir(main.join("sub")).expect("sub");
        let sub = main.join("sub");
        for dir in [&main, &worktree, &sub] {
            assert_eq!(
                repository(dir.to_str().expect("utf-8"), LIMIT).as_deref(),
                Some(main_str),
                "{}",
                dir.display()
            );
        }
        // A worktree since removed still belongs where it was.
        let gone = main.join(".claude/worktrees/landed");
        std::fs::create_dir_all(main.join(".claude/worktrees")).expect("worktrees");
        assert_eq!(
            repository(gone.to_str().expect("utf-8"), LIMIT).as_deref(),
            Some(main_str)
        );
        // A folder named relative to wherever the agent was is nowhere.
        assert_eq!(repository("wt", LIMIT), None);
        let plain = tempfile::tempdir().expect("dir");
        assert_eq!(
            repository(plain.path().to_str().expect("utf-8"), LIMIT),
            None
        );
    }

    #[test]
    fn commits_of_a_folder_that_is_not_a_repository_is_unknown_not_zero() {
        let plain = tempfile::tempdir().expect("dir");
        assert_eq!(
            commits(
                plain.path().to_str().expect("utf-8"),
                "2026-10-01",
                None,
                LIMIT
            ),
            None
        );
        assert_eq!(commits("/no/such/folder", "2026-10-01", None, LIMIT), None);
    }

    #[test]
    fn a_missing_gh_marks_the_code_stale_not_the_route() {
        let freshness = probe_with("/no/such/gh", Some(CHECKOUT));
        let line = freshness.line();
        assert!(line.starts_with("stale ·"), "{line}");
        assert!(line.contains("gh unavailable"), "{line}");
    }

    #[test]
    fn a_failing_gh_marks_the_code_stale() {
        let freshness = probe_with("/usr/bin/false", Some(CHECKOUT));
        let line = freshness.line();
        assert!(line.starts_with("stale ·"), "{line}");
        assert!(
            !line.contains("no checkout"),
            "the probe never reached gh: {line}"
        );
    }

    #[test]
    fn a_gh_that_does_not_answer_is_given_up_on() {
        // A hop is decided while a person waits on a stopped agent, and this
        // probe only feeds the pack's freshness line. On 2026-09-27 the first
        // organic hop reached its Codex thread 21.6 minutes after the decision,
        // and `gh` was one of the calls on that path with no limit at all.
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let gh = dir.path().join("gh");
        std::fs::write(&gh, "#!/bin/sh\nsleep 30\n").expect("write");
        std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).expect("chmod");

        let started = std::time::Instant::now();
        let line = probe_with(gh.to_str().expect("utf-8"), Some(CHECKOUT)).line();

        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "{line}"
        );
        assert!(line.starts_with("stale ·"), "{line}");
        assert!(line.contains("did not answer"), "{line}");
    }
}
