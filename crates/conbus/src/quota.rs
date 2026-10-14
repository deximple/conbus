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

use std::collections::BTreeSet;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Duration;

use conbus_core::clock::ago;
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
    match read_agy() {
        Ok(quota) => println!("agy       {}", describe(&quota, now)),
        Err(err) => println!("agy       unavailable — {err}"),
    }
    println!("claude    {}", describe_claude(paths, now));
    for name in claude_profiles(paths) {
        println!("{name:<9} {}", describe_claude_named(paths, &name, now));
    }
    for slug in conbus_core::attach::slugs() {
        if slug == Harness::CLAUDE_CODE || slug == Harness::CODEX || slug == Harness::AGY {
            continue;
        }
        let Some(caps) = conbus_core::attach::caps(slug) else {
            continue;
        };
        println!(
            "{slug:<9} {}",
            describe_attach(paths, slug, caps.quota_signal)
        );
    }
    Ok(())
}

/// Official print-mode JSON. Not a TUI scrape: the unmodified binary
/// already fetched remaining quota and emits `command.data.groups`.
pub fn read_agy() -> io::Result<Quota> {
    let binary = agy_binary();
    let output = Command::new(&binary)
        .args([
            "-p",
            "/usage",
            "--output-format",
            "json",
            "--print-timeout",
            "15s",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .map_err(|err| {
            io::Error::other(format!(
                "could not run `{} -p /usage`: {err}",
                binary.to_string_lossy()
            ))
        })?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "`{} -p /usage` exited {}",
            binary.to_string_lossy(),
            output.status
        )));
    }
    let value: serde_json::Value = serde_json::from_slice(&output.stdout)
        .map_err(|err| io::Error::other(format!("agy /usage was not JSON: {err}")))?;
    parse_agy(&value)
}

fn agy_binary() -> std::ffi::OsString {
    std::env::var_os("CONBUS_AGY_BIN")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "agy".into())
}

/// `agy -p "/usage" --output-format json`, captured on 1.1.27.
///
/// `remaining_fraction` is what is left. Hop reads `used_percent`, so
/// `round((1 - remaining) * 100)`. Primary is the tightest 5h bucket
/// across groups — that is the window that can refuse a turn first.
pub fn parse_agy(result: &serde_json::Value) -> io::Result<Quota> {
    let groups = result
        .pointer("/command/data/groups")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| {
            io::Error::other(format!("no command.data.groups in the reply: {result}"))
        })?;
    let mut five: Option<Window> = None;
    let mut week: Option<Window> = None;
    for group in groups {
        let Some(buckets) = group.get("buckets").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for bucket in buckets {
            let Some(window) = agy_bucket(bucket) else {
                continue;
            };
            match window.minutes {
                300 if five.is_none_or(|cur| window.used_percent > cur.used_percent) => {
                    five = Some(window);
                }
                10080 if week.is_none_or(|cur| window.used_percent > cur.used_percent) => {
                    week = Some(window);
                }
                _ => {}
            }
        }
    }
    if five.is_none() && week.is_none() {
        return Err(io::Error::other(
            "agy /usage named groups but no 5h or weekly bucket",
        ));
    }
    let reached = [five.as_ref(), week.as_ref()]
        .into_iter()
        .flatten()
        .any(|w| w.used_percent >= 100)
        .then(|| "rate_limit".to_string());
    Ok(Quota {
        plan: Some("agy".into()),
        primary: five.or(week),
        secondary: week.filter(|_| five.is_some()),
        reached,
        reset_credits: 0,
    })
}

fn agy_bucket(bucket: &serde_json::Value) -> Option<Window> {
    let remaining = bucket.get("remaining_fraction")?.as_f64()?;
    let minutes = match bucket.get("window")?.as_str()? {
        "5h" => 300,
        "weekly" => 10080,
        _ => return None,
    };
    let resets_at = unix_rfc3339(bucket.get("reset_time")?.as_str()?)?;
    let used_percent = ((1.0 - remaining) * 100.0).round().clamp(0.0, 100.0) as u64;
    Some(Window {
        used_percent,
        minutes,
        resets_at,
    })
}

/// `2026-09-06T13:47:28Z` — the only shape Agy 1.1.27 sent.
fn unix_rfc3339(stamp: &str) -> Option<i64> {
    let stamp = stamp.strip_suffix('Z')?;
    let (date, time) = stamp.split_once('T')?;
    let mut date = date.split('-');
    let year: i32 = date.next()?.parse().ok()?;
    let month: u32 = date.next()?.parse().ok()?;
    let day: u32 = date.next()?.parse().ok()?;
    let time = time.split('.').next()?;
    let mut time = time.split(':');
    let hour: u32 = time.next()?.parse().ok()?;
    let min: u32 = time.next()?.parse().ok()?;
    let sec: u32 = time.next()?.parse().ok()?;
    let days = days_from_civil(year, month, day)?;
    Some(days * 86_400 + i64::from(hour) * 3_600 + i64::from(min) * 60 + i64::from(sec))
}

/// Howard Hinnant's civil-from-days inverse, days since Unix epoch.
fn days_from_civil(year: i32, month: u32, day: u32) -> Option<i64> {
    if !(1..=12).contains(&month) || day == 0 || day > 31 {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = if year >= 0 { year } else { year - 399 } / 400;
    let yoe = (year - era * 400) as u32;
    let mp = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(i64::from(era) * 146_097 + i64::from(doe) - 719_468)
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
    read_cached_at(paths, harness, now_secs())
}

/// How long a cached reading still describes the account.
///
/// The tight window is five hours and the rule that consumes these numbers
/// turns on the 80% boundary, so a reading this old has had thirty per cent
/// of its own window to go wrong. Claude's numbers arrive only when the
/// person's StatusLine fires, which stops the moment they work anywhere else
/// — an afternoon in Codex used to leave the router deciding on the morning.
pub const CACHE_TTL_SECS: i64 = 90 * 60;

/// The cache as of `now`, with what has gone out of date removed.
///
/// Two different clocks. The *numbers* expire on the TTL, because nothing
/// refreshes them but the vendor. The *exhaustion* expires on the window's
/// own `resets_at`, because that is the vendor telling us exactly when it
/// stops being true — a five-hour window that reset an hour ago still read
/// EXHAUSTED, and routing away from a leg that is free again is the same
/// mistake as routing into one that is not.
pub fn read_cached_at(paths: &Paths, harness: &str, now: i64) -> Option<Quota> {
    let text = std::fs::read_to_string(paths.quota_cache(harness)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    let observed_at = value.get("observed_at").and_then(|v| v.as_i64());
    let fresh = observed_at.is_some_and(|at| now.saturating_sub(at) <= CACHE_TTL_SECS);
    let window = |name: &str| -> Option<Window> {
        if !fresh {
            return None;
        }
        let w = value.get(name)?;
        Some(Window {
            used_percent: w.get("used_percent")?.as_u64()?,
            minutes: w.get("minutes")?.as_u64()?,
            resets_at: w.get("resets_at")?.as_i64()?,
        })
    };
    let primary = window("primary");
    // An exhaustion is over when the window that caused it has reset, and
    // the cache does not say which window that was: `reached` is set if the
    // five-hour, the weekly *or* the spend cap is full, while `primary` is
    // whichever of them was recorded first. Expiring on `primary` alone
    // un-flags a weekly cap the moment the five-hour window rolls over, and
    // then the router hops into an account that is spent for another three
    // days — the wall `Wait` exists to prevent. So the exhaustion stands
    // while *any* window we recorded is still open, and a reading that
    // describes no window at all is never expired: not knowing when it ends
    // is not evidence that it has.
    let resets_at = |name: &str| {
        value
            .get(name)
            .and_then(|w| w.get("resets_at"))
            .and_then(|at| at.as_i64())
    };
    let windows = [resets_at("primary"), resets_at("secondary")];
    let still_reached = windows.iter().flatten().any(|at| *at > now)
        || windows.iter().all(std::option::Option::is_none);
    Some(Quota {
        plan: value
            .get("plan")
            .and_then(|v| v.as_str())
            .map(str::to_string),
        primary,
        secondary: window("secondary"),
        reached: value
            .get("reached")
            .and_then(|v| v.as_str())
            .filter(|_| still_reached)
            .map(str::to_string),
        reset_credits: 0,
    })
}

/// When the reading was taken, whether or not it is still current.
fn cached_observed_at(paths: &Paths, harness: &str) -> Option<i64> {
    let text = std::fs::read_to_string(paths.quota_cache(harness)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("observed_at").and_then(|v| v.as_i64())
}

fn write_cached(paths: &Paths, harness: &str, quota: &Quota) -> io::Result<()> {
    let path = paths.quota_cache(harness);
    // A status line can run before the recorder ever has.
    paths.keep_private()?;
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

/// Names-only scan of a hook payload. Used for attach legs that have no
/// remaining-quota JSON yet: we record that we looked, never the values.
pub fn observe_hook_keys(paths: &Paths, harness: &str, stdin: &str) {
    let Ok(serde_json::Value::Object(fields)) = serde_json::from_str::<serde_json::Value>(stdin)
    else {
        return;
    };
    let path = paths.quota_keys(harness);
    let mut names: BTreeSet<String> = fields.keys().cloned().collect();
    let mut quota_named = names.iter().any(|n| looks_like_quota_key(n));
    if let Ok(prev) = std::fs::read_to_string(&path)
        && let Ok(value) = serde_json::from_str::<serde_json::Value>(&prev)
    {
        if let Some(arr) = value.get("keys").and_then(|v| v.as_array()) {
            for key in arr {
                if let Some(name) = key.as_str() {
                    names.insert(name.to_string());
                }
            }
        }
        if value.get("quota_named").and_then(|v| v.as_bool()) == Some(true) {
            quota_named = true;
        }
    }
    let names: Vec<String> = names.into_iter().collect();
    // Best effort, like the rest of this: a hook never fails a turn, and a
    // root that cannot be closed is the recorder's to report.
    let _ = paths.keep_private();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let body = serde_json::json!({
        "schema_version": 1,
        "harness": harness,
        "observed_at": now_secs(),
        "keys": names,
        "quota_named": quota_named,
    });
    let tmp = path.with_extension("json.next");
    if std::fs::write(&tmp, serde_json::to_vec(&body).unwrap_or_default()).is_ok() {
        let _ = std::fs::rename(tmp, path);
    }
}

fn looks_like_quota_key(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    n.contains("rate_limit")
        || n.contains("ratelimit")
        || n.contains("used_percent")
        || n.contains("usedpercent")
        || n == "quota"
        || n == "remaining"
        || n == "usage"
}

fn describe_attach(paths: &Paths, slug: &str, quota_signal: bool) -> String {
    if quota_signal {
        return "official remaining-quota channel captured".into();
    }
    let Ok(text) = std::fs::read_to_string(paths.quota_keys(slug)) else {
        return "no official remaining-quota channel captured".into();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
        return "no official remaining-quota channel captured".into();
    };
    if value.get("quota_named").and_then(|v| v.as_bool()) == Some(true) {
        return "hook named a quota-like field — not parsed; we do not read vendor tokens to fetch one".into();
    }
    "hook keys scanned, no remaining-quota field".into()
}

/// Only the terminal `claude` runs a StatusLine command. The desktop app and
/// SDK sessions never do, so a person who works there has a live Claude and no
/// reading at all — measured on 2026-09-11, when 81% of Claude records came
/// from those two and the last reading was two days old.
const CLAUDE_SURFACES: &str =
    "only the terminal `claude` sends one; desktop and SDK sessions never do";

/// What doctor/quota say about the Claude StatusLine channel.
///
/// Two ages, because they answer different questions: how old the numbers are
/// says whether they still count, and when the StatusLine last ran says
/// whether the channel is alive at all. Reporting the first as the second told
/// a person their StatusLine "had not fired" for thirteen days while it had run
/// a week earlier with nothing to report.
/// The `claude_code@NAME` readings on this machine, sorted. A profile's
/// StatusLine writes beside the default's rather than into it, so each
/// account's quota is read against its own account.
fn claude_profiles(paths: &Paths) -> Vec<String> {
    let dir = paths.quota_cache("x");
    let Some(dir) = dir.parent() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|entry| entry.ok())
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let name = name
                .strip_suffix(".seen.json")
                .or_else(|| name.strip_suffix(".json"))?;
            name.starts_with("claude_code@").then(|| name.to_string())
        })
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

pub fn describe_claude(paths: &Paths, now: i64) -> String {
    describe_claude_named(paths, Harness::CLAUDE_CODE, now)
}

fn describe_claude_named(paths: &Paths, name: &str, now: i64) -> String {
    let ran = read_seen(paths, name)
        .map(|at| format!("StatusLine last ran {} ago", ago(now.saturating_sub(at))));
    if let Some(quota) = read_cached_at(paths, name, now) {
        let age = cached_observed_at(paths, name)
            .map(|at| now.saturating_sub(at))
            .unwrap_or_default();
        if age > CACHE_TTL_SECS {
            return format!(
                "{} · last reading {} ago, so the numbers are not counted{} · {CLAUDE_SURFACES}",
                describe(&quota, now),
                ago(age),
                ran.map(|ran| format!(" · {ran}")).unwrap_or_default()
            );
        }
        return describe(&quota, now);
    }
    match ran {
        Some(ran) => format!(
            "no rate_limits reading yet — {ran}; the vendor fills them after the first API \
             response · {CLAUDE_SURFACES}"
        ),
        None => format!(
            "StatusLine never ran — restart the terminal `claude` so it loads the wrapped \
             statusLine command · {CLAUDE_SURFACES}"
        ),
    }
}

fn read_seen(paths: &Paths, harness: &str) -> Option<i64> {
    let text = std::fs::read_to_string(paths.quota_seen(harness)).ok()?;
    let value: serde_json::Value = serde_json::from_str(&text).ok()?;
    value.get("observed_at")?.as_i64()
}

fn write_seen(paths: &Paths, harness: &str, has_rate_limits: bool) -> io::Result<()> {
    let path = paths.quota_seen(harness);
    paths.keep_private()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = serde_json::json!({
        "schema_version": 1,
        "harness": harness,
        "observed_at": now_secs(),
        "has_rate_limits": has_rate_limits,
    });
    let tmp = path.with_extension("json.next");
    std::fs::write(&tmp, serde_json::to_vec(&body).map_err(io::Error::other)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
}

/// Persist `rate_limits` from StatusLine stdin, then run the person's own
/// statusLine command if install wrapped one. Stdout is the status bar, never
/// hook JSON.
///
/// A statusLine child of a profiled `claude` inherits `CLAUDE_CONFIG_DIR`,
/// and its readings belong to that account: `profile` scopes the cache to
/// `quota/claude_code@NAME.json` so the default profile's file stays the
/// default profile's.
pub fn observe_statusline(
    paths: Option<&Paths>,
    forward: Option<&Path>,
    stdin: &str,
    profile: Option<&str>,
) -> String {
    if let Some(paths) = paths {
        let name = profile.map_or_else(
            || Harness::CLAUDE_CODE.to_string(),
            |name| format!("{}@{name}", Harness::CLAUDE_CODE),
        );
        let parsed = serde_json::from_str::<serde_json::Value>(stdin).ok();
        let quota = parsed.as_ref().and_then(parse_statusline);
        let _ = write_seen(paths, &name, quota.is_some());
        if let Some(quota) = quota {
            let _ = write_cached(paths, &name, &quota);
        }
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
        let payload =
            include_str!("../tests/fixtures/claude_code_2.1.263_statusline_rate_limits.json");

        let out = observe_statusline(Some(&paths), Some(&forward), payload, None);

        assert_eq!(out.trim(), "FORWARDED");
        let cached = read_cached(&paths, Harness::CLAUDE_CODE).expect("cache");
        assert_eq!(cached.primary.as_ref().map(|w| w.used_percent), Some(72));
        assert!(read_seen(&paths, Harness::CLAUDE_CODE).is_some());
    }

    #[test]
    fn a_statusline_without_limits_is_still_seen() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::from_env(root.path().join("conbus").to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        observe_statusline(Some(&paths), None, r#"{"cwd":"/repo"}"#, None);
        assert!(read_cached(&paths, Harness::CLAUDE_CODE).is_none());
        assert!(read_seen(&paths, Harness::CLAUDE_CODE).is_some());
        assert!(describe_claude(&paths, 0).contains("no rate_limits reading yet"));
    }

    #[test]
    fn a_profiled_statusline_writes_beside_the_default_reading() {
        // `CLAUDE_CONFIG_DIR=$HOME/.claude-work claude` puts the statusLine
        // child under the work account too: its reading lands in
        // `claude_code@work` files and lists as its own `conbus quota` line,
        // never mixed into the default account's.
        let root = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::from_env(root.path().join("conbus").to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        let payload =
            include_str!("../tests/fixtures/claude_code_2.1.263_statusline_rate_limits.json");

        observe_statusline(Some(&paths), None, payload, Some("work"));

        assert_eq!(
            claude_profiles(&paths),
            vec!["claude_code@work".to_string()]
        );
        let quota = read_cached(&paths, "claude_code@work").expect("profile cache");
        assert_eq!(quota.primary.as_ref().map(|w| w.used_percent), Some(72));
        assert!(read_seen(&paths, "claude_code@work").is_some());
        assert!(
            read_cached(&paths, Harness::CLAUDE_CODE).is_none(),
            "the default profile's file stays the default profile's"
        );
        let line = describe_claude_named(&paths, "claude_code@work", 1_788_385_869);
        assert!(line.contains("72% used"), "{line}");
    }

    #[test]
    fn a_status_line_or_hook_that_makes_the_root_makes_it_closed() {
        // Either can run before the recorder ever has: a status line on the
        // first refresh, a hook on the first turn.
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &std::path::Path| {
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };
        let home = tempfile::tempdir().expect("tempdir");

        let status =
            Paths::from_env(home.path().join("status").to_str(), None, None).expect("paths");
        observe_statusline(Some(&status), None, r#"{"cwd":"/repo"}"#, None);
        assert_eq!(mode(status.root()), 0o700, "the status line");

        let hook = Paths::from_env(home.path().join("hook").to_str(), None, None).expect("paths");
        observe_hook_keys(&hook, Harness::CURSOR, r#"{"cursor_version":"2"}"#);
        assert_eq!(mode(hook.root()), 0o700, "a hook");
    }

    #[test]
    fn attach_hook_keys_are_names_only_and_do_not_invent_a_signal() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::from_env(root.path().join("conbus").to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        observe_hook_keys(
            &paths,
            Harness::CURSOR,
            r#"{"cursor_version":"2","prompt":"secret","conversation_id":"c"}"#,
        );
        let text = std::fs::read_to_string(paths.quota_keys(Harness::CURSOR)).expect("keys");
        assert!(text.contains("cursor_version"));
        assert!(!text.contains("secret"));
        assert!(
            describe_attach(&paths, Harness::CURSOR, false).contains("no remaining-quota field")
        );
    }

    #[test]
    fn attach_hook_keys_keep_fields_seen_on_an_earlier_event() {
        let root = tempfile::tempdir().expect("tempdir");
        let paths =
            Paths::from_env(root.path().join("conbus").to_str(), None, None).expect("paths");
        std::fs::create_dir_all(paths.root()).expect("root");
        observe_hook_keys(
            &paths,
            Harness::CURSOR,
            r#"{"hook_event_name":"afterAgentResponse","text":"secret","cursor_version":"2"}"#,
        );
        observe_hook_keys(
            &paths,
            Harness::CURSOR,
            r#"{"hook_event_name":"preToolUse","tool_name":"Shell","cursor_version":"2"}"#,
        );
        let text = std::fs::read_to_string(paths.quota_keys(Harness::CURSOR)).expect("keys");
        assert!(text.contains("\"text\""), "{text}");
        assert!(text.contains("tool_name"), "{text}");
        assert!(!text.contains("secret"), "{text}");
    }

    #[test]
    fn the_agy_print_mode_contract_reads_as_two_windows() {
        let payload: serde_json::Value =
            serde_json::from_str(include_str!("../tests/fixtures/agy_1.1.27_usage.json"))
                .expect("fixture");
        let quota = parse_agy(&payload).expect("groups");

        assert_eq!(quota.plan.as_deref(), Some("agy"));
        assert_eq!(
            quota.primary,
            Some(Window {
                used_percent: 72,
                minutes: 300,
                resets_at: unix_rfc3339("2026-09-06T13:47:28Z").expect("stamp"),
            })
        );
        assert_eq!(quota.secondary.as_ref().map(|w| w.used_percent), Some(31));
        assert_eq!(quota.reached, None);
    }

    #[test]
    fn a_full_agy_week_is_reached_even_when_five_hours_remain() {
        let payload = serde_json::json!({
            "command": {"data": {"groups": [{
                "name": "Gemini Models",
                "buckets": [
                    {
                        "id": "gemini-5h",
                        "window": "5h",
                        "remaining_fraction": 0.5,
                        "reset_time": "2026-09-06T13:47:28Z"
                    },
                    {
                        "id": "gemini-weekly",
                        "window": "weekly",
                        "remaining_fraction": 0.0,
                        "reset_time": "2026-09-11T11:13:03Z"
                    }
                ]
            }]}}
        });
        let quota = parse_agy(&payload).expect("groups");
        assert_eq!(quota.primary.as_ref().map(|w| w.used_percent), Some(50));
        assert_eq!(quota.secondary.as_ref().map(|w| w.used_percent), Some(100));
        assert_eq!(quota.reached.as_deref(), Some("rate_limit"));
    }

    #[test]
    fn a_zero_agy_fraction_is_the_reached_signal() {
        let payload = serde_json::json!({
            "command": {"data": {"groups": [{
                "name": "Gemini Models",
                "buckets": [{
                    "id": "gemini-5h",
                    "window": "5h",
                    "remaining_fraction": 0.0,
                    "reset_time": "2026-09-06T13:47:28Z"
                }]
            }]}}
        });
        let quota = parse_agy(&payload).expect("groups");
        assert_eq!(quota.primary.as_ref().map(|w| w.used_percent), Some(100));
        assert_eq!(quota.reached.as_deref(), Some("rate_limit"));
    }

    #[test]
    fn agy_without_groups_is_an_error_not_a_zeroed_line() {
        assert!(parse_agy(&serde_json::json!({"command": {"name": "usage"}})).is_err());
    }

    #[test]
    fn rfc3339_z_is_unix_seconds() {
        assert_eq!(unix_rfc3339("2026-09-06T13:47:28Z"), Some(1_788_702_448));
    }

    /// A cache file in `write_cached`'s shape, with a chosen age.
    fn cached(paths: &conbus_core::paths::Paths, observed_at: i64, body: serde_json::Value) {
        let path = paths.quota_cache(conbus_core::event::Harness::CLAUDE_CODE);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("dir");
        let mut value = body;
        value["schema_version"] = serde_json::json!(1);
        value["observed_at"] = serde_json::json!(observed_at);
        std::fs::write(&path, value.to_string()).expect("write");
    }

    fn machine() -> (tempfile::TempDir, conbus_core::paths::Paths) {
        let dir = tempfile::tempdir().expect("tempdir");
        let paths =
            conbus_core::paths::Paths::from_env(dir.path().to_str(), None, None).expect("paths");
        (dir, paths)
    }

    #[test]
    fn a_stale_reading_keeps_the_plan_and_drops_the_numbers() {
        // The StatusLine only fires while the person is in Claude Code. An
        // afternoon spent in Codex left the router deciding on the morning's
        // percentages, and nothing said so.
        let (_dir, paths) = machine();
        let now = 1_800_000_000;
        cached(
            &paths,
            now - 4 * 3600,
            serde_json::json!({
                "plan": "max",
                "primary": {"used_percent": 95, "minutes": 300, "resets_at": now + 1800},
            }),
        );

        let quota = read_cached_at(&paths, Harness::CLAUDE_CODE, now).expect("cache");

        assert!(
            quota.primary.is_none(),
            "a four-hour-old number is not a number"
        );
        assert_eq!(
            quota.plan.as_deref(),
            Some("max"),
            "the plan does not go stale"
        );
    }

    #[test]
    fn a_fresh_reading_is_kept_whole() {
        let (_dir, paths) = machine();
        let now = 1_800_000_000;
        cached(
            &paths,
            now - 600,
            serde_json::json!({
                "plan": "max",
                "primary": {"used_percent": 42, "minutes": 300, "resets_at": now + 1800},
            }),
        );

        let quota = read_cached_at(&paths, Harness::CLAUDE_CODE, now).expect("cache");

        assert_eq!(quota.primary.expect("window").used_percent, 42);
    }

    #[test]
    fn a_window_that_has_since_reset_is_not_an_exhaustion() {
        // The vendor tells us exactly when it stops being true. Routing away
        // from a leg that is free again is the same mistake as routing into
        // one that is not.
        let (_dir, paths) = machine();
        let now = 1_800_000_000;
        cached(
            &paths,
            now - 6 * 3600,
            serde_json::json!({
                "plan": "max",
                "reached": "rate_limit",
                "primary": {"used_percent": 100, "minutes": 300, "resets_at": now - 3600},
            }),
        );

        let quota = read_cached_at(&paths, Harness::CLAUDE_CODE, now).expect("cache");

        assert!(quota.reached.is_none(), "the window reset an hour ago");
    }

    #[test]
    fn an_exhaustion_whose_window_is_still_open_survives() {
        let (_dir, paths) = machine();
        let now = 1_800_000_000;
        cached(
            &paths,
            now - 300,
            serde_json::json!({
                "plan": "max",
                "reached": "rate_limit",
                "primary": {"used_percent": 100, "minutes": 300, "resets_at": now + 1200},
            }),
        );

        let quota = read_cached_at(&paths, Harness::CLAUDE_CODE, now).expect("cache");

        assert_eq!(quota.reached.as_deref(), Some("rate_limit"));
    }

    #[test]
    fn doctor_says_when_the_statusline_reading_stopped_being_current() {
        let (_dir, paths) = machine();
        let now = 1_800_000_000;
        cached(
            &paths,
            now - 4 * 3600,
            serde_json::json!({
                "plan": "max",
                "primary": {"used_percent": 95, "minutes": 300, "resets_at": now + 1800},
            }),
        );

        let line = describe_claude(&paths, now);

        assert!(
            line.contains("last reading 4h ago, so the numbers are not counted"),
            "a stale reading must say how old it is: {line}"
        );
        assert!(
            !line.contains("StatusLine last ran"),
            "nothing says when the StatusLine ran, so the line must not guess: {line}"
        );
        assert!(line.contains("desktop and SDK sessions never do"), "{line}");
    }

    #[test]
    fn a_statusline_that_ran_without_limits_is_not_reported_as_silent() {
        // 2026-09-23: the reading was 13 days old, the StatusLine had last run
        // 7 days earlier with no `rate_limits`, and doctor said it "has not
        // fired for 19304m" — the age of the numbers, not of the channel.
        let (_dir, paths) = machine();
        // Real time on both sides: `write_seen` stamps the present.
        let now = now_secs();
        cached(
            &paths,
            now - 13 * 86_400,
            serde_json::json!({
                "plan": "claude",
                "primary": {"used_percent": 87, "minutes": 10080, "resets_at": now - 86_400},
            }),
        );
        write_seen(&paths, Harness::CLAUDE_CODE, false).expect("seen");

        let line = describe_claude(&paths, now);

        assert!(line.contains("StatusLine last ran 0s ago"), "{line}");
        assert!(line.contains("last reading 13d ago"), "{line}");
        assert!(!line.contains("has not fired"), "{line}");
    }

    #[test]
    fn a_weekly_cap_survives_the_five_hour_window_rolling_over() {
        // `reached` is set when the five-hour, the weekly or the spend cap
        // is full, and the cache does not record which. Expiring it on the
        // five-hour window alone told the router that an account spent for
        // another three days was available.
        let (_dir, paths) = machine();
        let now = 1_800_000_000;
        cached(
            &paths,
            now - 600,
            serde_json::json!({
                "plan": "max",
                "reached": "rate_limit",
                "primary": {"used_percent": 62, "minutes": 300, "resets_at": now - 60},
                "secondary": {"used_percent": 100, "minutes": 10080, "resets_at": now + 3 * 86400},
            }),
        );

        let quota = read_cached_at(&paths, Harness::CLAUDE_CODE, now).expect("cache");

        assert_eq!(
            quota.reached.as_deref(),
            Some("rate_limit"),
            "the weekly window has three days left"
        );
    }

    #[test]
    fn an_exhaustion_describing_no_window_is_never_expired() {
        // A spend cap sets `reached` and persists no window of its own. Not
        // knowing when it ends is not evidence that it has.
        let (_dir, paths) = machine();
        let now = 1_800_000_000;
        cached(
            &paths,
            now - 600,
            serde_json::json!({"plan": "max", "reached": "rate_limit"}),
        );

        let quota = read_cached_at(&paths, Harness::CLAUDE_CODE, now).expect("cache");

        assert_eq!(quota.reached.as_deref(), Some("rate_limit"));
    }
}
