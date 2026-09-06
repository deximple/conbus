//! Envelope encryption for tool bodies that stay on disk.
//!
//! The KEK never appears in the log and is never exposed as a model tool.
//! Tests set `CONBUS_KEK_HEX`. Live macOS uses the login Keychain.

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};

use crate::event::Body;

pub const ALG: &str = "aes-256-gcm";

/// 32-byte KEK, or none when sealing must fall back to elision.
pub fn kek() -> Option<[u8; 32]> {
    if let Ok(hex) = std::env::var("CONBUS_KEK_HEX") {
        return decode32(hex.trim());
    }
    #[cfg(target_os = "macos")]
    {
        keychain_kek()
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
    let existing = std::process::Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            "log-kek",
            "-s",
            "sh.conbus",
            "-w",
        ])
        .output()
        .ok()?;
    if existing.status.success() {
        let hex = String::from_utf8(existing.stdout).ok()?;
        return decode32(hex.trim());
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).ok()?;
    let hex = encode(&bytes);
    let ok = std::process::Command::new("security")
        .args([
            "add-generic-password",
            "-a",
            "log-kek",
            "-s",
            "sh.conbus",
            "-w",
            &hex,
        ])
        .status()
        .ok()?
        .success();
    ok.then_some(bytes)
}

#[cfg(test)]
pub(crate) static KEK_ENV: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

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
