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

use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use conbus_core::event::Harness;
use conbus_core::paths::Paths;

use crate::appserver::AppServer;

/// One rate-limit window.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
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

/// Ask a fresh app-server for the account's limits.
pub fn read() -> io::Result<Quota> {
    let mut server = AppServer::start(DEADLINE)?;
    let result = server.call("account/rateLimits/read", serde_json::json!({}))?;
    parse(&result)
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

/// The account in one line: the plan, each window, the credits, the signal.
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
    parts.join(" · ")
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

pub fn run(paths: &Paths) -> io::Result<()> {
    let now = now_secs();
    match read() {
        Ok(quota) => println!("codex     {}", describe(&quota, now)),
        Err(err) => println!("codex     unavailable — {err}"),
    }
    match read_cached(paths, Harness::CLAUDE_CODE) {
        Some(quota) => println!("claude    {}", describe(&quota, now)),
        None => println!(
            "claude    no StatusLine observation yet — open claude so the vendor fills rate_limits"
        ),
    }
    for slug in conbus_core::attach::slugs() {
        if slug == Harness::CLAUDE_CODE || slug == Harness::CODEX {
            continue;
        }
        let Some(caps) = conbus_core::attach::caps(slug) else {
            continue;
        };
        println!(
            "{slug:<9} {}",
            if caps.quota_signal {
                "official remaining-quota channel captured"
            } else {
                "no official remaining-quota channel captured"
            }
        );
    }
    Ok(())
}

/// Claude StatusLine stdin, as the 2.1.263 binary documents it.
///
/// The unmodified `claude` binary fetches `/api/oauth/usage` itself and puts
/// `rate_limits` on this payload. We persist the numbers. We never hold a
/// token and we never call that URL.
pub fn parse_statusline(payload: &serde_json::Value) -> Option<Quota> {
    let limits = payload.get("rate_limits")?;
    if !limits.is_object() {
        return None;
    }
    let window = |name: &str, minutes: u64| -> Option<Window> {
        let w = limits.get(name)?;
        Some(Window {
            used_percent: percent(w.get("used_percentage")?)?,
            minutes,
            resets_at: w.get("resets_at")?.as_i64()?,
        })
    };
    let five = window("five_hour", 300);
    let week = window("seven_day", 10080);
    let spend = window("spend_limit", 0);
    if five.is_none() && week.is_none() && spend.is_none() {
        return None;
    }
    let reached = [&five, &week, &spend]
        .into_iter()
        .flatten()
        .any(|w| w.used_percent >= 100)
        .then(|| "rate_limit".to_string());
    Some(Quota {
        plan: Some("claude".into()),
        primary: five.or(week).or(spend),
        secondary: week.filter(|_| five.is_some()),
        reached,
        reset_credits: 0,
    })
}

fn percent(value: &serde_json::Value) -> Option<u64> {
    value
        .as_f64()
        .map(|n| n.round().clamp(0.0, 10_000.0) as u64)
        .or_else(|| value.as_u64())
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Last observation the StatusLine command wrote, if any.
pub fn read_cached(paths: &Paths, harness: &str) -> Option<Quota> {
    let text = std::fs::read_to_string(paths.quota_cache(harness)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let window = |name: &str| -> Option<Window> {
        let w = value.get(name)?;
        Some(Window {
            used_percent: w.get("used_percent")?.as_u64()?,
            minutes: w.get("minutes")?.as_u64()?,
            resets_at: w.get("resets_at")?.as_i64()?,
        })
    };
    Some(Quota {
        plan: value
            .get("plan")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        primary: window("primary"),
        secondary: window("secondary"),
        reached: value
            .get("reached")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        reset_credits: 0,
    })
}

fn write_cached(paths: &Paths, harness: &str, quota: &Quota) -> io::Result<()> {
    let path = paths.quota_cache(harness);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({
        "schema_version": 1,
        "harness": harness,
        "observed_at": now_secs(),
        "plan": quota.plan,
        "primary": quota.primary.as_ref().map(|w| serde_json::json!({
            "used_percent": w.used_percent,
            "minutes": w.minutes,
            "resets_at": w.resets_at,
        })),
        "secondary": quota.secondary.as_ref().map(|w| serde_json::json!({
            "used_percent": w.used_percent,
            "minutes": w.minutes,
            "resets_at": w.resets_at,
        })),
        "reached": quota.reached,
    });
    let tmp = path.with_extension("json.next");
    std::fs::write(&tmp, serde_json::to_vec(&body).map_err(io::Error::other)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Persist `rate_limits` from StatusLine stdin, then run the person's own
/// statusLine command if install wrapped one. Stdout is the status bar, never
/// hook JSON.
pub fn observe_statusline(paths: Option<&Paths>, forward: Option<&Path>, stdin: &str) -> String {
    if let Some(paths) = paths
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(stdin)
        && let Some(quota) = parse_statusline(&value)
    {
        let _ = write_cached(paths, Harness::CLAUDE_CODE, &quota);
    }
    if let Some(path) = forward
        && let Ok(command) = std::fs::read_to_string(path)
        && !command.trim().is_empty()
    {
        return run_forward(&command, stdin.as_bytes());
    }
    String::new()
}

fn run_forward(command: &str, stdin: &[u8]) -> String {
    let mut child = match Command::new("/bin/sh")
        .arg("-c")
        .arg(command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return String::new(),
    };
    if let Some(mut pipe) = child.stdin.take() {
        let _ = pipe.write_all(stdin);
    }
    match child.wait_with_output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).into_owned(),
        Err(_) => String::new(),
    }
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
            "plan plus · 5h 22% used, resets in 1h 12m · weekly 9% used, resets in 4d 7h · reset credits 1 · not exhausted"
        );
    }

    #[test]
    fn an_exhausted_account_says_so_in_capitals() {
        // A 2026-09-03 guess used `"primary"` here. The first live exhaustion
        // (2026-09-06) sent `rate_limit_reached` — pin that string, not the guess.
        let mut reply = captured();
        reply["rateLimits"]["rateLimitReachedType"] = serde_json::json!("rate_limit_reached");

        let line = describe(&parse(&reply).expect("parse"), 0);

        assert!(line.contains("EXHAUSTED (rate_limit_reached)"), "{line}");
    }

    #[test]
    fn the_live_exhausted_reply_is_the_reached_signal() {
        let reply: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/codex_0.152.1_quota_exhausted.json"
        ))
        .expect("fixture");
        let quota = parse(&reply).expect("parse");

        assert_eq!(quota.reached.as_deref(), Some("rate_limit_reached"));
        assert_eq!(quota.primary.as_ref().map(|w| w.used_percent), Some(100));
        assert_eq!(quota.reset_credits, 3);
        assert!(describe(&quota, 1_788_626_797 - 5_400).contains("EXHAUSTED (rate_limit_reached)"));
    }

    #[test]
    fn a_reply_without_limits_is_an_error_not_a_zeroed_line() {
        assert!(parse(&serde_json::json!({"nothing": true})).is_err());
    }

    #[test]
    fn the_vendor_statusline_contract_reads_as_two_windows() {
        let payload: serde_json::Value = serde_json::from_str(include_str!(
            "../tests/fixtures/claude_code_2.1.263_statusline_rate_limits.json"
        ))
        .expect("fixture");
        let quota = parse_statusline(&payload).expect("rate_limits");

        assert_eq!(
            quota.primary,
            Some(Window {
                used_percent: 72,
                minutes: 300,
                resets_at: 1_788_385_869
            })
        );
        assert_eq!(quota.secondary.as_ref().map(|w| w.used_percent), Some(31));
        assert_eq!(quota.reached, None);
    }

    #[test]
    fn a_full_five_hour_window_is_the_reached_signal() {
        let payload = serde_json::json!({
            "rate_limits": {
                "five_hour": { "used_percentage": 100, "resets_at": 10 }
            }
        });
        let quota = parse_statusline(&payload).expect("rate_limits");
        assert_eq!(quota.reached.as_deref(), Some("rate_limit"));
    }

    #[test]
    fn statusline_without_rate_limits_is_none() {
        assert!(parse_statusline(&serde_json::json!({"cwd": "/repo"})).is_none());
    }

    #[test]
    fn observe_persists_and_forwards_the_persons_status_command() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::from_env(root.path().join("conbus").to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let forward = root.path().join("forward.sh");
        std::fs::write(&forward, "echo FORWARDED").expect("write");
        let payload = include_str!(
            "../tests/fixtures/claude_code_2.1.263_statusline_rate_limits.json"
        );

        let out = observe_statusline(Some(&paths), Some(&forward), payload);

        assert_eq!(out.trim(), "FORWARDED");
        let cached = read_cached(&paths, Harness::CLAUDE_CODE).expect("cache");
        assert_eq!(cached.primary.as_ref().map(|w| w.used_percent), Some(72));
    }
}
