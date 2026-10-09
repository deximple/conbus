//! Envelope encryption for tool bodies that stay on disk.
//!
//! The KEK never appears in the log and is never exposed as a model tool.
//! Tests set `CONBUS_KEK_HEX`. Live macOS uses the login Keychain.

#[cfg(any(target_os = "macos", test))]
use std::io::Read;
#[cfg(any(target_os = "macos", test))]
use std::process::{Command, Output, Stdio};
#[cfg(any(target_os = "macos", test))]
use std::sync::OnceLock;
#[cfg(any(target_os = "macos", test))]
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};

use crate::event::Body;

pub const ALG: &str = "aes-256-gcm";

/// How long `security` may take to answer. The recorder seals a tool body on
/// the path that writes it, while the hook that sent it waits.
#[cfg(any(target_os = "macos", test))]
const KEYCHAIN_LIMIT: Duration = Duration::from_secs(5);

/// The Keychain's KEK, once read. Asking for it per body started a `security`
/// process for every tool input and output the recorder wrote.
#[cfg(target_os = "macos")]
static KEYCHAIN_KEK: OnceLock<[u8; 32]> = OnceLock::new();

/// The key in `cell`, or `fetch`ed and kept there. A fetch that failed is not
/// kept: the next body asks again rather than being elided for good.
#[cfg(any(target_os = "macos", test))]
fn remembered(
    cell: &OnceLock<[u8; 32]>,
    fetch: impl FnOnce() -> Option<[u8; 32]>,
) -> Option<[u8; 32]> {
    if let Some(key) = cell.get() {
        return Some(*key);
    }
    let key = fetch()?;
    Some(*cell.get_or_init(|| key))
}

/// Run `command` to completion, or stop it once `limit` has passed. Its
/// output is a line or two, so reading it after exit cannot hold it up.
#[cfg(any(target_os = "macos", test))]
fn output_within(mut command: Command, limit: Duration) -> Option<Output> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    let mut stdout = Vec::new();
    child.stdout.take()?.read_to_end(&mut stdout).ok()?;
    Some(Output {
        status,
        stdout,
        stderr: Vec::new(),
    })
}

/// 32-byte KEK, or none when sealing must fall back to elision.
pub fn kek() -> Option<[u8; 32]> {
    if let Ok(hex) = std::env::var("CONBUS_KEK_HEX") {
        return decode32(hex.trim());
    }
    #[cfg(target_os = "macos")]
    {
        remembered(&KEYCHAIN_KEK, keychain_kek)
    }
    #[cfg(not(target_os = "macos"))]
    {
        None
    }
}

pub fn seal_text(text: &str) -> Option<Body> {
    let key = kek()?;
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let mut nonce_bytes = [0u8; 12];
    getrandom::getrandom(&mut nonce_bytes).ok()?;
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher.encrypt(nonce, text.as_bytes()).ok()?;
    Some(Body::Sealed {
        alg: ALG.into(),
        nonce: encode(&nonce_bytes),
        ciphertext: encode(&ciphertext),
        original_bytes: text.len(),
    })
}

pub fn open(body: &Body) -> Option<String> {
    let Body::Sealed {
        alg,
        nonce,
        ciphertext,
        ..
    } = body
    else {
        return None;
    };
    if alg != ALG {
        return None;
    }
    let key = kek()?;
    let nonce = decode(nonce)?;
    let ciphertext = decode(ciphertext)?;
    if nonce.len() != 12 {
        return None;
    }
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(&key));
    let nonce = Nonce::from_slice(&nonce);
    let plain = cipher.decrypt(nonce, ciphertext.as_slice()).ok()?;
    String::from_utf8(plain).ok()
}

/// Decrypt a sealed body for an explicit reveal. Other kinds pass through.
pub fn reveal(body: &Body) -> Body {
    match body {
        Body::Sealed { .. } => match open(body) {
            Some(text) => Body::kept(text),
            None => body.clone(),
        },
        other => other.clone(),
    }
}

fn encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn decode(hex: &str) -> Option<Vec<u8>> {
    if !hex.len().is_multiple_of(2) {
        return None;
    }
    let mut out = Vec::with_capacity(hex.len() / 2);
    let bytes = hex.as_bytes();
    for chunk in bytes.chunks(2) {
        let hi = from_hex(chunk[0])?;
        let lo = from_hex(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Some(out)
}

fn decode32(hex: &str) -> Option<[u8; 32]> {
    let bytes = decode(hex)?;
    <[u8; 32]>::try_from(bytes).ok()
}

fn from_hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(target_os = "macos")]
fn keychain_kek() -> Option<[u8; 32]> {
    let mut find = Command::new("security");
    find.args([
        "find-generic-password",
        "-a",
        "log-kek",
        "-s",
        "sh.conbus",
        "-w",
    ]);
    let existing = output_within(find, KEYCHAIN_LIMIT)?;
    if existing.status.success() {
        let hex = String::from_utf8(existing.stdout).ok()?;
        return decode32(hex.trim());
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).ok()?;
    let hex = encode(&bytes);
    let mut add = Command::new("security");
    add.args([
        "add-generic-password",
        "-a",
        "log-kek",
        "-s",
        "sh.conbus",
        "-w",
        &hex,
    ]);
    let ok = output_within(add, KEYCHAIN_LIMIT)?.status.success();
    ok.then_some(bytes)
}

#[cfg(test)]
pub(crate) static KEK_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_fetched_once_is_not_fetched_again() {
        let cell = std::sync::OnceLock::new();
        let fetches = std::cell::Cell::new(0);
        let fetch = || {
            fetches.set(fetches.get() + 1);
            Some([7u8; 32])
        };
        assert_eq!(remembered(&cell, fetch), Some([7u8; 32]));
        assert_eq!(remembered(&cell, fetch), Some([7u8; 32]));
        assert_eq!(fetches.get(), 1);
    }

    #[test]
    fn a_key_that_could_not_be_fetched_is_asked_for_again() {
        let cell = std::sync::OnceLock::new();
        assert_eq!(remembered(&cell, || None), None);
        assert_eq!(remembered(&cell, || Some([9u8; 32])), Some([9u8; 32]));
    }

    #[test]
    fn a_keychain_that_does_not_answer_is_given_up_on() {
        let mut sleep = std::process::Command::new("sleep");
        sleep.arg("30");
        let started = std::time::Instant::now();
        assert!(output_within(sleep, std::time::Duration::from_millis(200)).is_none());
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
        let mut echo = std::process::Command::new("echo");
        echo.arg("abc");
        let out = output_within(echo, std::time::Duration::from_secs(5)).expect("echo");
        assert_eq!(String::from_utf8_lossy(&out.stdout).trim(), "abc");
    }

    #[test]
    fn a_sealed_body_round_trips_under_a_test_kek() {
        let _guard = KEK_ENV.lock().expect("kek env");
        // SAFETY: held under KEK_ENV so no concurrent set_var.
        unsafe { std::env::set_var("CONBUS_KEK_HEX", "aa".repeat(32)) };
        let sealed = seal_text("tool stdout").expect("sealed");
        assert!(matches!(sealed, Body::Sealed { .. }));
        assert_eq!(open(&sealed).as_deref(), Some("tool stdout"));
        assert_eq!(
            reveal(&sealed),
            Body::Kept {
                text: "tool stdout".into()
            }
        );
        unsafe { std::env::remove_var("CONBUS_KEK_HEX") };
    }
}
