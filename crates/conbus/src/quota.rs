//! `conbus quota` — the plan's probe #1, as a command.
//!
//! The router's whole premise is that a harness's quota can be read before it
//! is hit. On 2026-09-03 Codex's app-server answered `account/rateLimits/read`
//! for a client that was not a Codex session at all: the account's five-hour
//! and weekly windows, how much of each is used, when each resets, the plan,
//! and `rateLimitReachedType` — the "exhausted" signal — all without opening a
//! thread. The shape is in `tests/fixtures/codex_0.152.1_contract.md`.
//!
//! Read-only. It starts an app-server, asks, and stops it; nothing about the
//! account changes.

use std::io::{self, BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One rate-limit window.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Window {
    pub used_percent: u64,
    pub minutes: u64,
    /// Unix seconds.
    pub resets_at: i64,
}

/// What the account is allowed, as the app-server reports it.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct Quota {
    pub plan: Option<String>,
    pub primary: Option<Window>,
    pub secondary: Option<Window>,
    /// Set when a limit is hit — the signal the router routes on.
    pub reached: Option<String>,
    pub reset_credits: u64,
}

/// How long the app-server gets to answer before it is stopped. The real one
/// answered in well under a second; MCP servers it tries to start are what
/// can hang it, and they are not our question.
pub const DEADLINE: Duration = Duration::from_secs(15);

/// Which binary answers. `CONBUS_CODEX_BIN` overrides `codex` on `PATH`, for
/// tests and for a Codex installed somewhere the daemon's `PATH` does not see.
fn codex_binary() -> std::ffi::OsString {
    std::env::var_os("CONBUS_CODEX_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "codex".into())
}

/// Ask a fresh app-server for the account's limits.
pub fn read() -> io::Result<Quota> {
    let binary = codex_binary();
    let mut child = Command::new(&binary)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| {
            io::Error::other(format!(
                "could not run `{} app-server`: {err}",
                binary.to_string_lossy()
            ))
        })?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("no stdin"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("no stdout"))?;

    // A watchdog rather than a read timeout: the reply is a few lines, and
    // what can stall is the server itself. Stopping it unblocks the read.
    let child = Arc::new(Mutex::new(child));
    let watched = Arc::clone(&child);
    std::thread::spawn(move || {
        std::thread::sleep(DEADLINE);
        if let Ok(mut child) = watched.lock() {
            let _ = child.kill();
        }
    });

    let answer = ask(&mut stdin, BufReader::new(stdout));
    drop(stdin);
    if let Ok(mut child) = child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
    answer
}

/// The JSON-RPC exchange: `initialize`, the `initialized` notification, then
/// the one question. Anything the server says in between is skipped.
fn ask(stdin: &mut impl Write, stdout: impl BufRead) -> io::Result<Quota> {
    for line in [
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
            "clientInfo": {"name": "conbus", "title": "conbus quota probe", "version": crate::build_info::BUILD},
            "capabilities": {"experimentalApi": true}}}),
        serde_json::json!({"jsonrpc": "2.0", "method": "initialized", "params": {}}),
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "account/rateLimits/read", "params": {}}),
    ] {
        writeln!(stdin, "{line}")?;
        stdin.flush()?;
    }
    for line in stdout.lines() {
        let line = line?;
        let Ok(message) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        if message["id"] != 2 {
            continue;
        }
        if let Some(error) = message.get("error") {
            return Err(io::Error::other(format!(
                "codex app-server refused account/rateLimits/read: {error}"
            )));
        }
        return parse(&message["result"]);
    }
    Err(io::Error::other(
        "codex app-server closed without answering account/rateLimits/read",
    ))
}

/// Read the captured shape. Tolerant: a field the server stops sending
/// becomes `None`, not a failure, because the numbers that are there are
/// still worth printing.
pub fn parse(result: &serde_json::Value) -> io::Result<Quota> {
    let limits = result
        .get("rateLimits")
        .ok_or_else(|| io::Error::other(format!("no rateLimits in the reply: {result}")))?;
    let window = |name: &str| -> Option<Window> {
        let w = limits.get(name)?;
        Some(Window {
            used_percent: w.get("usedPercent")?.as_u64()?,
            minutes: w.get("windowDurationMins")?.as_u64()?,
            resets_at: w.get("resetsAt")?.as_i64()?,
        })
    };
    Ok(Quota {
        plan: limits
            .get("planType")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        primary: window("primary"),
        secondary: window("secondary"),
        reached: limits
            .get("rateLimitReachedType")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        reset_credits: result
            .get("rateLimitResetCredits")
            .and_then(|c| c.get("availableCount"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0),
    })
}

/// One line, the way `doctor` would say it.
pub fn describe(quota: &Quota, now: i64) -> String {
    let mut parts = vec![format!(
        "plan {}",
        quota.plan.as_deref().unwrap_or("unknown")
    )];
    for window in [&quota.primary, &quota.secondary].into_iter().flatten() {
        parts.push(format!(
            "{} {}% used, {}",
            label(window.minutes),
            window.used_percent,
            resets(window.resets_at - now)
        ));
    }
    parts.push(format!("reset credits {}", quota.reset_credits));
    parts.push(match &quota.reached {
        Some(kind) => format!("EXHAUSTED ({kind})"),
        None => "not exhausted".to_string(),
    });
    format!("codex     {}", parts.join(" · "))
}

fn label(minutes: u64) -> String {
    match minutes {
        10080 => "weekly".to_string(),
        m if m % 60 == 0 => format!("{}h", m / 60),
        m => format!("{m}m"),
    }
}

fn resets(seconds: i64) -> String {
    if seconds <= 0 {
        return "reset due".to_string();
    }
    let (d, h, m) = (
        seconds / 86_400,
        (seconds % 86_400) / 3_600,
        (seconds % 3_600) / 60,
    );
    match (d, h, m) {
        (0, 0, m) => format!("resets in {m}m"),
        (0, h, m) => format!("resets in {h}h {m}m"),
        (d, h, _) => format!("resets in {d}d {h}h"),
    }
}

pub fn run() -> io::Result<()> {
    let quota = read()?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    println!("{}", describe(&quota, now));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The reply captured on 2026-09-03, account id and timestamps aside.
    fn captured() -> serde_json::Value {
        serde_json::json!({"rateLimits": {"limitId": "codex", "limitName": null,
            "primary": {"usedPercent": 22, "windowDurationMins": 300, "resetsAt": 1_788_385_869},
            "secondary": {"usedPercent": 9, "windowDurationMins": 10080, "resetsAt": 1_788_752_527},
            "credits": {"hasCredits": false, "unlimited": false, "balance": "0"},
            "individualLimit": null, "spendControlReached": false, "planType": "plus",
            "rateLimitReachedType": null},
            "rateLimitResetCredits": {"availableCount": 1, "credits": []},
            "accountId": "00000000-0000-0000-0000-000000000000", "rateLimitUpsell": null})
    }

    #[test]
    fn the_captured_reply_reads_as_two_windows_a_plan_and_no_exhaustion() {
        let quota = parse(&captured()).expect("parse");

        assert_eq!(quota.plan.as_deref(), Some("plus"));
        assert_eq!(
            quota.primary,
            Some(Window {
                used_percent: 22,
                minutes: 300,
                resets_at: 1_788_385_869
            })
        );
        assert_eq!(quota.secondary.as_ref().map(|w| w.minutes), Some(10080));
        assert_eq!(quota.reached, None);
        assert_eq!(quota.reset_credits, 1);
    }

    #[test]
    fn the_line_names_the_windows_and_the_signal() {
        let quota = parse(&captured()).expect("parse");

        let line = describe(&quota, 1_788_385_869 - 4_320);

        assert_eq!(
            line,
            "codex     plan plus · 5h 22% used, resets in 1h 12m · weekly 9% used, resets in 4d 7h · reset credits 1 · not exhausted"
        );
    }

    #[test]
    fn an_exhausted_account_says_so_in_capitals() {
        let mut reply = captured();
        reply["rateLimits"]["rateLimitReachedType"] = serde_json::json!("primary");

        let line = describe(&parse(&reply).expect("parse"), 0);

        assert!(line.contains("EXHAUSTED (primary)"), "{line}");
    }

    #[test]
    fn a_reply_without_limits_is_an_error_not_a_zeroed_line() {
        assert!(parse(&serde_json::json!({"nothing": true})).is_err());
    }
}
