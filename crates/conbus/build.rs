//! Stamps the binary with the commit it was built from, so `conbus --version`,
//! the daemon's start-up banner and `doctor` can all say which code is
//! running. The first audit had to run `strings` on the installed binary to
//! guess; the deploy after the second opened a window where doctor and the
//! daemon ran different code with nothing to say so.
//!
//! Falls back to `unknown` outside a git checkout and never fails the build.
//! `CONBUS_BUILD` in the environment overrides everything, for builds made
//! from an export rather than a checkout.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8(out.stdout).ok()?.trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn main() {
    println!("cargo:rerun-if-env-changed=CONBUS_BUILD");
    // Rerun when the commit moves. The paths are resolved through git so a
    // worktree, whose `.git` is a file pointing elsewhere, is followed too.
    // The dirty flag is only refreshed on those occasions and on a clean
    // build; a stamp is a commit, not a diff.
    for path in ["HEAD", "packed-refs"] {
        if let Some(resolved) = git(&["rev-parse", "--git-path", path]) {
            println!("cargo:rerun-if-changed={resolved}");
        }
    }
    if let Some(branch) = git(&["symbolic-ref", "-q", "HEAD"])
        && let Some(resolved) = git(&["rev-parse", "--git-path", &branch])
    {
        println!("cargo:rerun-if-changed={resolved}");
    }

    let build = std::env::var("CONBUS_BUILD")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| match git(&["rev-parse", "--short=9", "HEAD"]) {
            Some(sha) => {
                let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some();
                if dirty { format!("{sha}+dirty") } else { sha }
            }
            None => "unknown".to_string(),
        });
    println!("cargo:rustc-env=CONBUS_BUILD={build}");
}
