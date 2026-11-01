//! `conbus sync`: the device side of RFD 0002 and RFD 0003.
//!
//! This module never opens the log. Callers hand it `SharedRow`s from
//! `usage`. Nothing here starts a service, and a response cannot add a field
//! the invite did not already allow.

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use conbus_core::clock::Timestamp;
use conbus_core::paths::{Paths, keep_dir_private};
use conbus_core::usage::{SharedRow, next_day};
use ed25519_dalek::{Signer, SigningKey};
use serde::Serialize;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

const KEYCHAIN_SERVICE: &str = "sh.conbus";
const KEYCHAIN_ACCOUNT: &str = "sync-device-key";
const CURL: &str = "/usr/bin/curl";
const SCHEMA: u64 = 1;
const MAX_ROWS: usize = 64;
const MAX_BODY: usize = 64 * 1024;
const MAX_REQUESTS: u64 = 10;
const RETENTION_DAYS: i64 = 90;
const DELAY_MAX: u64 = 3600;

#[cfg(test)]
const ROW_KEYS: &[&str] = &[
    "account_stops",
    "attendance",
    "briefings",
    "conversations",
    "day",
    "harness",
    "hops",
    "limit_episodes",
    "limit_stops",
    "other_stops",
    "permission_asks",
    "recovered",
    "repeated",
    "request_faults",
    "subagents",
    "tokens_cache_read",
    "tokens_cache_write",
    "tokens_input",
    "tokens_output",
    "tool_calls",
    "tool_denials",
    "tool_failures",
    "tool_interrupts",
    "tool_ms",
    "turns",
    "user_stops",
    "vendor_errors",
];

/// What `conbus sync` was asked to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Args {
    pub join: Option<String>,
    pub apply: bool,
    pub off: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
struct Settings {
    #[serde(default)]
    omit_harnesses: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
struct Sent {
    revision: u64,
    digest: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
struct DayCount {
    day: String,
    count: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, serde::Deserialize)]
struct State {
    device: String,
    origin: String,
    org: String,
    settings: Settings,
    sent: BTreeMap<String, Sent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pause_until: Option<String>,
    requests: DayCount,
}

struct Invite {
    org: String,
    org_name: String,
    origin: String,
    region: String,
    member: String,
    expires: String,
    settings: Settings,
}

struct Post {
    url: String,
    body: Vec<u8>,
    signature_hex: String,
}

trait KeyStore {
    fn available(&self) -> bool;
    fn get(&self) -> io::Result<Option<[u8; 32]>>;
    fn put(&mut self, seed: &[u8; 32]) -> io::Result<()>;
    fn delete(&mut self) -> io::Result<()>;
}

trait Transport {
    fn post(&mut self, post: &Post) -> io::Result<Vec<u8>>;
}

/// Whether this machine has joined. A caller can skip computing `rows`
/// when it is false: an unjoined `sync` posts nothing.
pub fn has_state(paths: &Paths) -> bool {
    state_path(paths).exists()
}

pub fn run(paths: &Paths, args: Args, rows: &[SharedRow]) -> io::Result<()> {
    let now = Timestamp::now();
    #[cfg(target_os = "macos")]
    let mut keys = Keychain;
    #[cfg(not(target_os = "macos"))]
    let mut keys = RefusingKeys;
    let mut transport = Curl;
    let lines = execute(
        paths,
        &args,
        rows,
        now,
        &mut keys,
        &mut transport,
        random_seed,
    )?;
    for line in lines {
        println!("{line}");
    }
    Ok(())
}

fn execute(
    paths: &Paths,
    args: &Args,
    rows: &[SharedRow],
    now: Timestamp,
    keys: &mut dyn KeyStore,
    transport: &mut dyn Transport,
    mut new_seed: impl FnMut() -> io::Result<[u8; 32]>,
) -> io::Result<Vec<String>> {
    if args.off {
        if args.apply || args.join.is_some() {
            return Err(invalid("--off does not take --join or --apply"));
        }
        return turn_off(paths, now, keys, transport);
    }
    if let Some(raw) = &args.join {
        let invite = parse_invite(raw)?;
        return if args.apply {
            join_apply(paths, &invite, raw, now, keys, transport, &mut new_seed)
        } else {
            Ok(preview(&invite, &day_of(now)))
        };
    }
    if args.apply {
        return Err(invalid("--apply belongs with --join"));
    }
    send_due(paths, rows, now, keys, transport)
}

fn preview(invite: &Invite, today: &str) -> Vec<String> {
    let mut lines = vec![
        "join would write nothing".to_string(),
        format!("org: {}", invite.org),
        format!("name: {}", invite.org_name),
        format!("origin: {}", invite.origin),
        format!("region: {}", invite.region),
        format!("member: {}", invite.member),
        format!("expires: {}", invite.expires),
        format!("omit: {}", invite.settings.omit_harnesses.join(", ")),
    ];
    if invite.expires.as_str() < today {
        lines.push("expires is past; --apply will refuse".to_string());
    }
    lines
}

fn join_apply(
    paths: &Paths,
    invite: &Invite,
    raw: &str,
    now: Timestamp,
    keys: &mut dyn KeyStore,
    transport: &mut dyn Transport,
    new_seed: &mut impl FnMut() -> io::Result<[u8; 32]>,
) -> io::Result<Vec<String>> {
    let today = day_of(now);
    if invite.expires.as_str() < today.as_str() {
        return Err(invalid(format!("invite expired on {}", invite.expires)));
    }
    if !keys.available() {
        return Err(invalid(
            "--apply needs the login keychain, which this build does not have",
        ));
    }
    let mut seed = new_seed()?;
    let body = join_body(raw, &public_key(&seed))?;
    let signed = signed_post(&invite.origin, &body, &seed);
    let response = match transport.post(&signed) {
        Ok(response) => response,
        Err(err) => {
            wipe(&mut seed);
            return Err(err);
        }
    };
    let device = match device_id(&response) {
        Ok(device) => device,
        Err(err) => {
            wipe(&mut seed);
            return Err(err);
        }
    };
    if let Err(err) = keys.put(&seed) {
        wipe(&mut seed);
        return Err(err);
    }
    wipe(&mut seed);
    let state = State {
        device: device.clone(),
        origin: invite.origin.clone(),
        org: invite.org.clone(),
        settings: invite.settings.clone(),
        sent: BTreeMap::new(),
        pause_until: None,
        requests: DayCount {
            day: today,
            count: 1,
        },
    };
    write_state(&state_path(paths), &state)?;
    Ok(vec![format!("joined {} as device {device}", invite.org)])
}

fn send_due(
    paths: &Paths,
    rows: &[SharedRow],
    now: Timestamp,
    keys: &mut dyn KeyStore,
    transport: &mut dyn Transport,
) -> io::Result<Vec<String>> {
    let path = state_path(paths);
    let Some(mut state) = load_state(&path)? else {
        return Ok(vec![
            "this machine has not joined. conbus sync --join INVITE prints an invite and writes nothing"
                .to_string(),
        ]);
    };
    check_origin(&state.origin)?;
    let today = day_of(now);
    if let Some(until) = &state.pause_until {
        if now.to_string() < format!("{until}T00:00:00.000Z") {
            return Ok(vec![format!("paused until {until}")]);
        }
        state.pause_until = None;
    }
    let Some(seed) = keys.get()? else {
        return Err(invalid("joined, but the device key is not in the keychain"));
    };
    let mut seed = seed;
    let mut lines = Vec::new();
    let grouped = rows_by_day(rows, &state.settings.omit_harnesses);
    for (day, day_rows) in grouped {
        if !due(&day, &today, now, &state.device) {
            continue;
        }
        let Some(prepared) = prepare(&state.device, &day, &day_rows, state.sent.get(&day))? else {
            continue;
        };
        if budget_left(&state, &today) == 0 {
            lines.push("stopped: 10 requests today".to_string());
            break;
        }
        let post = signed_post(&state.origin, &prepared.body, &seed);
        let response = match transport.post(&post) {
            Ok(response) => response,
            Err(err) => {
                charge(&mut state, &today);
                let _ = write_state(&path, &state);
                wipe(&mut seed);
                return Err(err);
            }
        };
        charge(&mut state, &today);
        match read_answer(&response) {
            Answer::Revoked => {
                wipe(&mut seed);
                let _ = keys.delete();
                let _ = fs::remove_file(&path);
                lines.push(
                    "the service revoked this device. the key and sync state are deleted. the log is kept"
                        .to_string(),
                );
                return Ok(lines);
            }
            Answer::Pause(until) => {
                state.pause_until = Some(until.clone());
                write_state(&path, &state)?;
                lines.push(format!("paused until {until}"));
                break;
            }
            Answer::Accepted(revision) if revision == prepared.revision => {
                state.sent.insert(
                    day.clone(),
                    Sent {
                        revision: prepared.revision,
                        digest: prepared.digest,
                    },
                );
                write_state(&path, &state)?;
                lines.push(format!("sent {day} revision {}", prepared.revision));
            }
            Answer::Accepted(_) | Answer::Other => {
                write_state(&path, &state)?;
                lines.push(format!("{day} was not accepted"));
            }
        }
    }
    wipe(&mut seed);
    if lines.is_empty() {
        lines.push("nothing to send".to_string());
    }
    Ok(lines)
}

fn turn_off(
    paths: &Paths,
    now: Timestamp,
    keys: &mut dyn KeyStore,
    transport: &mut dyn Transport,
) -> io::Result<Vec<String>> {
    let path = state_path(paths);
    let state = load_state(&path)?;
    let mut note = None;
    if let (Some(state), Some(mut seed)) = (state.as_ref(), keys.get()?) {
        let body = leave_body(&state.device)?;
        let post = signed_post(&state.origin, &body, &seed);
        if let Err(err) = transport.post(&post) {
            note = Some(format!(
                "the service did not acknowledge leaving ({err}). the key is deleted anyway"
            ));
        }
        wipe(&mut seed);
        let _ = now;
    }
    keys.delete()?;
    if path.exists() {
        fs::remove_file(&path)?;
    }
    let mut lines = vec![
        "deleted the device key and sync state".to_string(),
        format!("the log is still at {}", paths.log_dir().display()),
        "ask the organisation to delete what was sent. the receipt names the organisation, the device, the day range, the number of rows removed, and the day the last backup holding them expires. conbus does not write that receipt".to_string(),
    ];
    if let Some(note) = note {
        lines.insert(0, note);
    }
    Ok(lines)
}

struct Prepared {
    revision: u64,
    body: Vec<u8>,
    digest: String,
}

fn prepare(
    device: &str,
    day: &str,
    rows: &[&SharedRow],
    prior: Option<&Sent>,
) -> io::Result<Option<Prepared>> {
    if rows.len() > MAX_ROWS {
        return Ok(None);
    }
    let same_revision = prior.map(|sent| sent.revision).unwrap_or(1);
    let same_body = envelope_bytes(device, day, same_revision, rows)?;
    if same_body.len() > MAX_BODY {
        return Ok(None);
    }
    let same_digest = digest(&same_body);
    if prior.is_some_and(|sent| sent.digest == same_digest) {
        return Ok(None);
    }
    let revision = prior
        .map(|sent| sent.revision.saturating_add(1))
        .unwrap_or(1);
    let body = if revision == same_revision {
        same_body
    } else {
        let body = envelope_bytes(device, day, revision, rows)?;
        if body.len() > MAX_BODY {
            return Ok(None);
        }
        body
    };
    let digest = digest(&body);
    Ok(Some(Prepared {
        revision,
        body,
        digest,
    }))
}

fn due(day: &str, today: &str, now: Timestamp, device: &str) -> bool {
    if day >= today {
        return false;
    }
    let Some(oldest) = days_before(today, RETENTION_DAYS) else {
        return false;
    };
    if day < oldest.as_str() {
        return false;
    }
    let Some(earliest) = earliest_send(day, device) else {
        return false;
    };
    now.to_string() >= earliest
}

fn earliest_send(day: &str, device: &str) -> Option<String> {
    let next = next_day(day)?;
    let start: Timestamp = format!("{next}T00:00:00.000Z").parse().ok()?;
    let epoch = Timestamp::from_unix_seconds(0)?;
    let at =
        Timestamp::from_unix_seconds(start.seconds_since(epoch) + delay_secs(device, day) as i64)?;
    Some(at.to_string())
}

fn delay_secs(device: &str, day: &str) -> u64 {
    let mut hasher = Sha256::new();
    hasher.update(device.as_bytes());
    hasher.update(day.as_bytes());
    let digest = hasher.finalize();
    let mut wide = [0u8; 8];
    wide.copy_from_slice(&digest[..8]);
    u64::from_be_bytes(wide) % DELAY_MAX
}

fn rows_by_day<'a>(rows: &'a [SharedRow], omit: &[String]) -> BTreeMap<String, Vec<&'a SharedRow>> {
    let mut grouped = BTreeMap::new();
    for row in rows {
        if omit.iter().any(|name| name == &row.harness) || !is_day(&row.day) {
            continue;
        }
        grouped
            .entry(row.day.clone())
            .or_insert_with(Vec::new)
            .push(row);
    }
    grouped
}

fn envelope_bytes(
    device: &str,
    day: &str,
    revision: u64,
    rows: &[&SharedRow],
) -> io::Result<Vec<u8>> {
    let envelope = Envelope {
        device,
        schema: SCHEMA,
        day_closed: day,
        revision,
        rows,
    };
    serde_json::to_vec(&envelope).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

#[derive(Serialize)]
struct Envelope<'a> {
    device: &'a str,
    schema: u64,
    day_closed: &'a str,
    revision: u64,
    rows: &'a [&'a SharedRow],
}

fn join_body(invite: &str, public_key: &str) -> io::Result<Vec<u8>> {
    let body = JoinBody { invite, public_key };
    serde_json::to_vec(&body).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

#[derive(Serialize)]
struct JoinBody<'a> {
    invite: &'a str,
    public_key: &'a str,
}

fn leave_body(device: &str) -> io::Result<Vec<u8>> {
    let body = LeaveBody {
        device,
        leave: true,
    };
    serde_json::to_vec(&body).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))
}

#[derive(Serialize)]
struct LeaveBody<'a> {
    device: &'a str,
    leave: bool,
}

fn signed_post(origin: &str, body: &[u8], seed: &[u8; 32]) -> Post {
    Post {
        url: origin.to_string(),
        signature_hex: signature_hex(seed, body),
        body: body.to_vec(),
    }
}

fn public_key(seed: &[u8; 32]) -> String {
    hex(&SigningKey::from_bytes(seed).verifying_key().to_bytes())
}

fn signature_hex(seed: &[u8; 32], body: &[u8]) -> String {
    hex(&SigningKey::from_bytes(seed).sign(body).to_bytes())
}

enum Answer {
    Accepted(u64),
    Revoked,
    Pause(String),
    Other,
}

fn read_answer(body: &[u8]) -> Answer {
    let Ok(value) = serde_json::from_slice::<Value>(body) else {
        return Answer::Other;
    };
    if value.get("revoked").and_then(Value::as_bool) == Some(true) {
        return Answer::Revoked;
    }
    if let Some(day) = value.get("pause_until").and_then(Value::as_str)
        && is_day(day)
    {
        return Answer::Pause(day.to_string());
    }
    if value.get("accepted").and_then(Value::as_bool) == Some(true) {
        return value
            .get("revision")
            .and_then(Value::as_u64)
            .map_or(Answer::Other, Answer::Accepted);
    }
    Answer::Other
}

fn device_id(body: &[u8]) -> io::Result<String> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|err| invalid(format!("join response was not json: {err}")))?;
    let Some(device) = value.get("device").and_then(Value::as_str) else {
        return Err(invalid("join response has no device id"));
    };
    if device.len() == 32
        && device
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        Ok(device.to_string())
    } else {
        Err(invalid("device id must be 32 lowercase hex characters"))
    }
}

fn parse_invite(raw: &str) -> io::Result<Invite> {
    let value: Value =
        serde_json::from_str(raw).map_err(|err| invalid(format!("invite is not json: {err}")))?;
    let obj = value
        .as_object()
        .ok_or_else(|| invalid("invite must be a json object"))?;
    let origin = required(obj, "origin")?;
    check_origin(&origin)?;
    let region = required(obj, "region")?;
    if region != "us" && region != "eu" {
        return Err(invalid("region must be us or eu"));
    }
    let expires = required(obj, "expires")?;
    if !is_day(&expires) {
        return Err(invalid("expires must be YYYY-MM-DD"));
    }
    Ok(Invite {
        org: required(obj, "org")?,
        org_name: required(obj, "org_name")?,
        origin,
        region,
        member: required(obj, "member")?,
        expires,
        settings: settings_of(obj.get("settings")),
    })
}

fn settings_of(value: Option<&Value>) -> Settings {
    let Some(obj) = value.and_then(Value::as_object) else {
        return Settings {
            omit_harnesses: Vec::new(),
        };
    };
    let omit_harnesses = obj
        .get("omit_harnesses")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    Settings { omit_harnesses }
}

fn required(obj: &Map<String, Value>, key: &str) -> io::Result<String> {
    obj.get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| invalid(format!("invite is missing {key}")))
}

fn check_origin(origin: &str) -> io::Result<()> {
    let Some(rest) = origin.strip_prefix("https://") else {
        return Err(invalid(
            "origin must be https://, with no user, query or fragment",
        ));
    };
    if rest.is_empty()
        || origin.contains('@')
        || origin.contains('?')
        || origin.contains('#')
        || rest.starts_with('/')
    {
        return Err(invalid(
            "origin must be https://, with no user, query or fragment",
        ));
    }
    let host = rest.split('/').next().unwrap_or("");
    if host.is_empty() {
        return Err(invalid(
            "origin must be https://, with no user, query or fragment",
        ));
    }
    Ok(())
}

fn budget_left(state: &State, today: &str) -> u64 {
    let used = if state.requests.day == today {
        state.requests.count
    } else {
        0
    };
    MAX_REQUESTS.saturating_sub(used)
}

fn charge(state: &mut State, today: &str) {
    if state.requests.day != today {
        state.requests = DayCount {
            day: today.to_string(),
            count: 0,
        };
    }
    state.requests.count += 1;
}

fn state_path(paths: &Paths) -> PathBuf {
    paths.root().join("sync.json")
}

fn load_state(path: &Path) -> io::Result<Option<State>> {
    match fs::read(path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map(Some)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err)),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err),
    }
}

fn write_state(path: &Path, state: &State) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        keep_dir_private(parent)?;
    }
    let text =
        serde_json::to_vec(state).map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
    let mut file = {
        let mut options = OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?
    };
    file.write_all(&text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

/// `-q` has to be the very first argument: it is the only way to stop curl
/// reading `~/.curlrc` (or `$CURL_HOME`/`$XDG_CONFIG_HOME`'s), and one line in
/// that file — `insecure`, `location`, `proxy = …` — would undo the transport
/// rules of RFD 0003 (D5). The rest pins them here instead: HTTPS only, TLS 1.2
/// or newer, and no redirect is ever followed.
fn curl_args() -> &'static [&'static str] {
    [
        "-q",
        "--config",
        "-",
        "--silent",
        "--show-error",
        "--max-time",
        "30",
        "--proto",
        "=https",
        "--tlsv1.2",
        "--max-redirs",
        "0",
    ]
    .as_slice()
}

fn curl_config(post: &Post) -> String {
    format!(
        "url = {}\nrequest = \"POST\"\nheader = \"Content-Type: application/json\"\nheader = \"Conbus-Signature: {}\"\ndata-binary = {}\n",
        curl_quote(&post.url),
        post.signature_hex,
        curl_quote(&String::from_utf8_lossy(&post.body))
    )
}

fn curl_quote(value: &str) -> String {
    let mut out = String::from("\"");
    for ch in value.chars() {
        match ch {
            '\\' | '"' => {
                out.push('\\');
                out.push(ch);
            }
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

struct Curl;

impl Transport for Curl {
    fn post(&mut self, post: &Post) -> io::Result<Vec<u8>> {
        if post.url.starts_with("http://") {
            return Err(invalid(
                "origin must be https://, with no user, query or fragment",
            ));
        }
        let mut child = Command::new(CURL)
            .args(curl_args())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        child
            .stdin
            .take()
            .ok_or_else(|| invalid("curl stdin was not piped"))?
            .write_all(curl_config(post).as_bytes())?;
        let output = child.wait_with_output()?;
        if !output.status.success() {
            let detail = String::from_utf8_lossy(&output.stderr);
            let detail = detail.trim();
            return Err(invalid(if detail.is_empty() {
                "curl failed".to_string()
            } else {
                format!("curl failed: {detail}")
            }));
        }
        Ok(output.stdout)
    }
}

#[cfg(target_os = "macos")]
struct Keychain;

#[cfg(target_os = "macos")]
impl KeyStore for Keychain {
    fn available(&self) -> bool {
        true
    }

    fn get(&self) -> io::Result<Option<[u8; 32]>> {
        match security_framework::passwords::get_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        ) {
            Ok(bytes) if bytes.len() == 32 => {
                let mut seed = [0u8; 32];
                seed.copy_from_slice(&bytes);
                Ok(Some(seed))
            }
            Ok(_) => Err(invalid("device key in the keychain is not 32 bytes")),
            Err(err) if err.code() == -25300 => Ok(None),
            Err(err) => Err(io::Error::other(err)),
        }
    }

    fn put(&mut self, seed: &[u8; 32]) -> io::Result<()> {
        let _ = security_framework::passwords::delete_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        );
        security_framework::passwords::set_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
            seed,
        )
        .map_err(io::Error::other)
    }

    fn delete(&mut self) -> io::Result<()> {
        match security_framework::passwords::delete_generic_password(
            KEYCHAIN_SERVICE,
            KEYCHAIN_ACCOUNT,
        ) {
            Ok(()) => Ok(()),
            Err(err) if err.code() == -25300 => Ok(()),
            Err(err) => Err(io::Error::other(err)),
        }
    }
}

#[cfg(any(test, not(target_os = "macos")))]
struct RefusingKeys;

#[cfg(any(test, not(target_os = "macos")))]
impl KeyStore for RefusingKeys {
    fn available(&self) -> bool {
        false
    }

    fn get(&self) -> io::Result<Option<[u8; 32]>> {
        Ok(None)
    }

    fn put(&mut self, _seed: &[u8; 32]) -> io::Result<()> {
        Err(invalid(
            "--apply needs the login keychain, which this build does not have",
        ))
    }

    fn delete(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn random_seed() -> io::Result<[u8; 32]> {
    let mut seed = [0u8; 32];
    getrandom::getrandom(&mut seed).map_err(|err| io::Error::other(err.to_string()))?;
    Ok(seed)
}

fn digest(body: &[u8]) -> String {
    hex(&Sha256::digest(body))
}

fn hex(bytes: &[u8]) -> String {
    const ALPHA: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(ALPHA[(byte >> 4) as usize] as char);
        out.push(ALPHA[(byte & 0xf) as usize] as char);
    }
    out
}

fn wipe(seed: &mut [u8; 32]) {
    *seed = [0; 32];
    std::sync::atomic::compiler_fence(std::sync::atomic::Ordering::SeqCst);
}

fn day_of(now: Timestamp) -> String {
    let text = now.to_string();
    text.get(..10).unwrap_or(&text).to_string()
}

fn is_day(day: &str) -> bool {
    format!("{day}T00:00:00.000Z").parse::<Timestamp>().is_ok() && day.len() == 10
}

fn days_before(day: &str, days: i64) -> Option<String> {
    let start: Timestamp = format!("{day}T00:00:00.000Z").parse().ok()?;
    let epoch = Timestamp::from_unix_seconds(0)?;
    Timestamp::from_unix_seconds(start.seconds_since(epoch) - days * 86_400).map(day_of)
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use conbus_core::usage::{Attendance, Counts};

    struct MemoryKey {
        seed: Option<[u8; 32]>,
    }

    impl KeyStore for MemoryKey {
        fn available(&self) -> bool {
            true
        }
        fn get(&self) -> io::Result<Option<[u8; 32]>> {
            Ok(self.seed)
        }
        fn put(&mut self, seed: &[u8; 32]) -> io::Result<()> {
            self.seed = Some(*seed);
            Ok(())
        }
        fn delete(&mut self) -> io::Result<()> {
            self.seed = None;
            Ok(())
        }
    }

    struct Recorder {
        posts: Vec<Post>,
        bodies: Vec<Vec<u8>>,
    }

    impl Transport for Recorder {
        fn post(&mut self, post: &Post) -> io::Result<Vec<u8>> {
            self.posts.push(Post {
                url: post.url.clone(),
                body: post.body.clone(),
                signature_hex: post.signature_hex.clone(),
            });
            Ok(self
                .bodies
                .pop()
                .unwrap_or_else(|| br#"{"accepted":true,"revision":1}"#.to_vec()))
        }
    }

    fn now() -> Timestamp {
        "2026-11-01T12:00:00.000Z".parse().unwrap()
    }

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn paths(home: &tempfile::TempDir) -> Paths {
        Paths::from_env(Some(home.path().to_str().unwrap()), None, None).unwrap()
    }

    fn invite(origin: &str) -> String {
        format!(
            r#"{{"org":"acme","org_name":"Acme","origin":"{origin}","region":"eu","member":"ada","expires":"2026-12-01","settings":{{"omit_harnesses":["cursor"],"workspaces":true}}}}"#
        )
    }

    fn row(day: &str, harness: &str, turns: u64) -> SharedRow {
        SharedRow {
            day: day.into(),
            harness: harness.into(),
            attendance: Attendance::Attended,
            conversations: 1,
            counts: Counts {
                turns,
                ..Counts::default()
            },
        }
    }

    const SEED: [u8; 32] = [0xa5; 32];

    fn run_at(
        paths: &Paths,
        args: Args,
        rows: &[SharedRow],
        keys: &mut dyn KeyStore,
        transport: &mut dyn Transport,
    ) -> io::Result<Vec<String>> {
        execute(paths, &args, rows, now(), keys, transport, || Ok(SEED))
    }

    #[test]
    fn an_envelope_has_only_the_contract_keys_and_rows_match_rfd_0002() {
        let body =
            envelope_bytes("ab", "2026-10-31", 1, &[&row("2026-10-31", "codex", 2)]).unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        let mut keys: Vec<&str> = value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort_unstable();
        assert_eq!(keys, ["day_closed", "device", "revision", "rows", "schema"]);
        assert_eq!(value["schema"], 1);
        let row = &value["rows"][0];
        let mut row_keys: Vec<&str> = row
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        row_keys.sort_unstable();
        assert_eq!(row_keys, ROW_KEYS);
        assert!(row.get("workspace").is_none(), "{row}");
    }

    #[test]
    fn an_open_day_is_not_sent_and_a_day_is_sent_once_per_revision() {
        let dir = home();
        let paths = paths(&dir);
        let mut keys = MemoryKey { seed: Some(SEED) };
        let mut transport = Recorder {
            posts: Vec::new(),
            bodies: vec![br#"{"accepted":true,"revision":1}"#.to_vec()],
        };
        write_state(
            &state_path(&paths),
            &State {
                device: "ab".repeat(16),
                origin: "https://sync.example".into(),
                org: "acme".into(),
                settings: Settings {
                    omit_harnesses: vec![],
                },
                sent: BTreeMap::new(),
                pause_until: None,
                requests: DayCount {
                    day: "2026-11-01".into(),
                    count: 0,
                },
            },
        )
        .unwrap();
        let rows = vec![row("2026-11-01", "codex", 1), row("2026-10-31", "codex", 2)];
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &rows,
            &mut keys,
            &mut transport,
        )
        .unwrap();
        assert_eq!(transport.posts.len(), 1);
        assert!(
            !transport.posts[0]
                .body
                .windows(10)
                .any(|window| window == b"2026-11-01")
        );
        let first = transport.posts[0].body.clone();
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &rows,
            &mut keys,
            &mut transport,
        )
        .unwrap();
        assert_eq!(transport.posts.len(), 1, "the same body is not sent again");
        let changed = vec![row("2026-10-31", "codex", 9)];
        transport
            .bodies
            .push(br#"{"accepted":true,"revision":2}"#.to_vec());
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &changed,
            &mut keys,
            &mut transport,
        )
        .unwrap();
        assert_eq!(transport.posts.len(), 2);
        let second: Value = serde_json::from_slice(&transport.posts[1].body).unwrap();
        assert_eq!(second["revision"], 2);
        assert_ne!(first, transport.posts[1].body);
    }

    #[test]
    fn unknown_response_fields_do_not_widen_settings() {
        let dir = home();
        let paths = paths(&dir);
        let mut keys = MemoryKey { seed: Some(SEED) };
        let mut transport = Recorder {
            posts: Vec::new(),
            bodies: vec![
                br#"{"accepted":true,"revision":1,"settings":{"omit_harnesses":[],"workspaces":true},"command":"rm"}"#
                    .to_vec(),
            ],
        };
        write_state(
            &state_path(&paths),
            &State {
                device: "ab".repeat(16),
                origin: "https://sync.example".into(),
                org: "acme".into(),
                settings: Settings {
                    omit_harnesses: vec!["cursor".into()],
                },
                sent: BTreeMap::new(),
                pause_until: None,
                requests: DayCount {
                    day: "2026-11-01".into(),
                    count: 0,
                },
            },
        )
        .unwrap();
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &[row("2026-10-31", "codex", 1)],
            &mut keys,
            &mut transport,
        )
        .unwrap();
        let saved: Value = serde_json::from_slice(&fs::read(state_path(&paths)).unwrap()).unwrap();
        assert_eq!(saved["settings"]["omit_harnesses"][0], "cursor");
        assert!(saved["settings"].get("workspaces").is_none());
        assert!(saved.get("command").is_none());
    }

    #[test]
    fn a_pause_blocks_the_transport_and_a_refusal_is_not_recorded_as_sent() {
        let dir = home();
        let paths = paths(&dir);
        let mut keys = MemoryKey { seed: Some(SEED) };
        let mut transport = Recorder {
            posts: Vec::new(),
            bodies: Vec::new(),
        };
        write_state(
            &state_path(&paths),
            &State {
                device: "ab".repeat(16),
                origin: "https://sync.example".into(),
                org: "acme".into(),
                settings: Settings {
                    omit_harnesses: vec![],
                },
                sent: BTreeMap::new(),
                pause_until: Some("2026-11-02".into()),
                requests: DayCount {
                    day: "2026-11-01".into(),
                    count: 0,
                },
            },
        )
        .unwrap();
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &[row("2026-10-31", "codex", 1)],
            &mut keys,
            &mut transport,
        )
        .unwrap();
        assert!(transport.posts.is_empty());

        let mut state = load_state(&state_path(&paths)).unwrap().unwrap();
        state.pause_until = None;
        write_state(&state_path(&paths), &state).unwrap();
        transport
            .bodies
            .push(br#"{"nope":true,"settings":{"workspaces":true}}"#.to_vec());
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &[row("2026-10-31", "codex", 1)],
            &mut keys,
            &mut transport,
        )
        .unwrap();
        let saved = load_state(&state_path(&paths)).unwrap().unwrap();
        assert!(saved.sent.is_empty());
        assert!(saved.settings.omit_harnesses.is_empty());
    }

    #[test]
    fn off_forgets_the_key_and_a_later_sync_does_not_post() {
        let dir = home();
        let paths = paths(&dir);
        let mut keys = MemoryKey { seed: Some(SEED) };
        let mut transport = Recorder {
            posts: Vec::new(),
            bodies: vec![br#"{"ok":true}"#.to_vec()],
        };
        write_state(
            &state_path(&paths),
            &State {
                device: "cd".repeat(16),
                origin: "https://sync.example".into(),
                org: "acme".into(),
                settings: Settings {
                    omit_harnesses: vec![],
                },
                sent: BTreeMap::new(),
                pause_until: None,
                requests: DayCount {
                    day: "2026-11-01".into(),
                    count: 0,
                },
            },
        )
        .unwrap();
        let lines = run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: true,
            },
            &[],
            &mut keys,
            &mut transport,
        )
        .unwrap();
        assert!(keys.get().unwrap().is_none());
        assert!(!state_path(&paths).exists());
        assert!(
            lines
                .iter()
                .any(|line| line.contains("does not write that receipt"))
        );
        assert!(lines.iter().any(|line| line.contains("log is still")));
        transport.posts.clear();
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &[row("2026-10-31", "codex", 1)],
            &mut keys,
            &mut transport,
        )
        .unwrap();
        assert!(transport.posts.is_empty());
    }

    #[test]
    fn the_seed_is_not_in_curl_config_or_sync_json() {
        let dir = home();
        let paths = paths(&dir);
        let mut keys = MemoryKey { seed: Some(SEED) };
        let mut transport = Recorder {
            posts: Vec::new(),
            bodies: vec![br#"{"accepted":true,"revision":1}"#.to_vec()],
        };
        write_state(
            &state_path(&paths),
            &State {
                device: "ab".repeat(16),
                origin: "https://sync.example".into(),
                org: "acme".into(),
                settings: Settings {
                    omit_harnesses: vec![],
                },
                sent: BTreeMap::new(),
                pause_until: None,
                requests: DayCount {
                    day: "2026-11-01".into(),
                    count: 0,
                },
            },
        )
        .unwrap();
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &[row("2026-10-31", "codex", 1)],
            &mut keys,
            &mut transport,
        )
        .unwrap();
        let seed_hex = hex(&SEED);
        let args = curl_args().join(" ");
        let config = curl_config(&transport.posts[0]);
        let stored = fs::read_to_string(state_path(&paths)).unwrap();
        assert!(!args.contains(&seed_hex), "{args}");
        assert!(!config.contains(&seed_hex), "{config}");
        assert!(!stored.contains(&seed_hex), "{stored}");
        assert_eq!(CURL, "/usr/bin/curl");
        assert!(config.contains("Conbus-Signature"));
        assert!(!config.contains("http://"));
    }

    #[test]
    fn curl_ignores_every_curlrc_and_cannot_be_redirected_or_downgraded() {
        // curl reads ~/.curlrc unless `-q` is its very first argument, and one
        // line there (`insecure`, `location`, `proxy = …`) would undo RFD 0003
        // D5. The body goes as `data-binary` because `data` treats a leading
        // `@` as a file name: the bytes sent must be the bytes signed.
        let args = curl_args();
        assert_eq!(args[0], "-q", "{args:?}");
        let joined = args.join(" ");
        for wanted in ["--proto =https", "--tlsv1.2", "--max-redirs 0"] {
            assert!(joined.contains(wanted), "missing {wanted}: {joined}");
        }
        for unwanted in ["-L", "--location", "-k", "--insecure", "--proxy"] {
            assert!(!args.contains(&unwanted), "{unwanted} in {joined}");
        }
        let config = curl_config(&Post {
            url: "https://sync.example/v1/batch".into(),
            signature_hex: "00".into(),
            body: b"@/etc/passwd".to_vec(),
        });
        assert!(
            config.contains("\ndata-binary = \"@/etc/passwd\"\n"),
            "{config}"
        );
        assert!(!config.contains("\ndata = "), "{config}");
    }

    #[test]
    fn a_hostile_curlrc_is_never_read() {
        // A real curl with our arguments, a home whose .curlrc routes everything
        // through a "proxy" we listen on, and a URL that refuses connections.
        // Read, the file would send curl to our listener; ignored, curl fails
        // straight away on the refused port and nothing ever connects.
        use std::net::TcpListener;
        if !std::path::Path::new(CURL).exists() {
            panic!("{CURL} is missing; this test needs the curl the transport runs");
        }
        let proxy = TcpListener::bind("127.0.0.1:0").expect("bind proxy");
        proxy.set_nonblocking(true).expect("nonblocking");
        let closed = TcpListener::bind("127.0.0.1:0").expect("bind closed");
        let closed_port = closed.local_addr().expect("addr").port();
        drop(closed);
        let home = home();
        let rc = format!(
            "proxy = \"http://127.0.0.1:{}\"\ninsecure\nlocation\n",
            proxy.local_addr().expect("proxy addr").port()
        );
        for dir in [home.path().to_path_buf(), home.path().join(".config")] {
            fs::create_dir_all(&dir).expect("dir");
            fs::write(dir.join(".curlrc"), &rc).expect("curlrc");
        }
        let mut child = Command::new(CURL)
            .args(curl_args())
            .env("HOME", home.path())
            .env("CURL_HOME", home.path())
            .env("XDG_CONFIG_HOME", home.path().join(".config"))
            .env_remove("HTTPS_PROXY")
            .env_remove("https_proxy")
            .env_remove("ALL_PROXY")
            .env_remove("all_proxy")
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn curl");
        let config = curl_config(&Post {
            url: format!("https://127.0.0.1:{closed_port}/v1/batch"),
            signature_hex: "00".into(),
            body: b"{}".to_vec(),
        });
        child
            .stdin
            .take()
            .expect("stdin")
            .write_all(config.as_bytes())
            .expect("write config");
        let status = child.wait().expect("curl exit");
        assert!(!status.success(), "a refused port must fail");
        assert!(
            proxy.accept().is_err(),
            "curl connected to the proxy named only in .curlrc"
        );
    }

    #[test]
    fn http_is_refused_before_the_transport_runs() {
        let dir = home();
        let paths = paths(&dir);
        let mut keys = MemoryKey { seed: None };
        let mut transport = Recorder {
            posts: Vec::new(),
            bodies: Vec::new(),
        };
        let err = run_at(
            &paths,
            Args {
                join: Some(invite("http://sync.example")),
                apply: false,
                off: false,
            },
            &[],
            &mut keys,
            &mut transport,
        )
        .unwrap_err();
        assert!(err.to_string().contains("https://"), "{err}");
        assert!(transport.posts.is_empty());
        assert!(!state_path(&paths).exists());
    }

    #[test]
    fn an_omitted_harness_is_absent_from_the_body() {
        let dir = home();
        let paths = paths(&dir);
        let mut keys = MemoryKey { seed: Some(SEED) };
        let mut transport = Recorder {
            posts: Vec::new(),
            bodies: vec![br#"{"accepted":true,"revision":1}"#.to_vec()],
        };
        write_state(
            &state_path(&paths),
            &State {
                device: "ab".repeat(16),
                origin: "https://sync.example".into(),
                org: "acme".into(),
                settings: Settings {
                    omit_harnesses: vec!["cursor".into()],
                },
                sent: BTreeMap::new(),
                pause_until: None,
                requests: DayCount {
                    day: "2026-11-01".into(),
                    count: 0,
                },
            },
        )
        .unwrap();
        run_at(
            &paths,
            Args {
                join: None,
                apply: false,
                off: false,
            },
            &[
                row("2026-10-31", "cursor", 4),
                row("2026-10-31", "codex", 2),
            ],
            &mut keys,
            &mut transport,
        )
        .unwrap();
        let body = String::from_utf8(transport.posts[0].body.clone()).unwrap();
        assert!(body.contains("codex"), "{body}");
        assert!(!body.contains("cursor"), "{body}");
    }

    #[test]
    fn preview_writes_nothing_and_apply_without_a_keychain_fails() {
        let dir = home();
        let paths = paths(&dir);
        let mut keys = RefusingKeys;
        let mut transport = Recorder {
            posts: Vec::new(),
            bodies: Vec::new(),
        };
        let lines = run_at(
            &paths,
            Args {
                join: Some(invite("https://sync.example")),
                apply: false,
                off: false,
            },
            &[],
            &mut keys,
            &mut transport,
        )
        .unwrap();
        assert!(lines.iter().any(|line| line.contains("write nothing")));
        assert!(transport.posts.is_empty());
        assert!(!state_path(&paths).exists());
        let err = run_at(
            &paths,
            Args {
                join: Some(invite("https://sync.example")),
                apply: true,
                off: false,
            },
            &[],
            &mut keys,
            &mut transport,
        )
        .unwrap_err();
        assert!(err.to_string().contains("keychain"), "{err}");
        assert!(transport.posts.is_empty());
    }

    #[test]
    fn sync_json_is_owner_readable_only() {
        let dir = home();
        let paths = paths(&dir);
        write_state(
            &state_path(&paths),
            &State {
                device: "ab".repeat(16),
                origin: "https://sync.example".into(),
                org: "acme".into(),
                settings: Settings {
                    omit_harnesses: vec![],
                },
                sent: BTreeMap::new(),
                pause_until: None,
                requests: DayCount {
                    day: "2026-11-01".into(),
                    count: 0,
                },
            },
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(state_path(&paths))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "{mode:o}");
    }
}
