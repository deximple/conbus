//! How fresh the checkout is, for the pack's `code_freshness` line.
//!
//! `gh` is consulted when a hop is decided. A failure is not a routing
//! failure: the hop is recorded and the pack says the code is stale.

use std::path::Path;
use std::process::Command;

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
    let out = Command::new("git")
        .args(["-C", dir, "rev-parse", "--short", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!sha.is_empty()).then_some(sha)
}

fn gh_note(gh_bin: &str, dir: &str) -> Result<String, String> {
    let out = Command::new(gh_bin)
        .args(["pr", "view", "--json", "state,number"])
        .current_dir(dir)
        .output()
        .map_err(|err| format!("gh unavailable ({err})"))?;
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
        assert!(!line.contains("no checkout"), "the probe never reached gh: {line}");
    }
}
