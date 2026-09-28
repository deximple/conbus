//! How fresh the checkout is, for the pack's `code_freshness` line.
//!
//! `gh` is consulted when a hop is decided. A failure is not a routing
//! failure: the hop is recorded and the pack says the code is stale.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// How long `git` or `gh` may take before the probe gives up on it.
///
/// A hop is decided while a person waits on an agent that just stopped, and
/// this probe only feeds a freshness line. On 2026-09-27 the first organic hop
/// reached its Codex thread 21.6 minutes after the decision, with `gh` and
/// `git` on that path and no limit on either.
pub const LIMIT: Duration = Duration::from_secs(5);

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
fn output_within(mut command: Command, limit: Duration) -> Result<Output, String> {
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
                let _ = child.wait();
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
