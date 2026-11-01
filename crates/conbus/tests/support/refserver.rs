//! A test-only reference sync server for RFD 0003. Never compiled into the
//! binary: it lives under `tests/support/` and implements just what the
//! client needs — join, signed batch, leave, revoked, pause_until — with the
//! one rule the spec pins hardest: the Ed25519 signature is verified against
//! the exact body bytes before the body is read.
//!
//! Plain HTTP on 127.0.0.1: TLS is documented at the bottom of this file —
//! `conbus`'s real transport pins `https://`, which needs rustls+rcgen wired
//! to `CURL_CA_BUNDLE`; that piece is not landed.

// Only the sync tests use this module; it is dead code in every other
// integration test that shares `tests/support`.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

#[derive(Default)]
pub struct RefServer {
    /// invite -> org, for join. The join response is the device id issued.
    pub invites: HashMap<String, String>,
    /// public key hex -> device id, in join order.
    pub devices: HashMap<String, String>,
    /// device -> (day, highest revision accepted). A revision only ever
    /// goes up; an old or replayed batch is refused.
    pub revisions: HashMap<String, (String, u64)>,
    /// device -> revoked. From then on its signatures answer `revoked`.
    pub revoked: HashMap<String, bool>,
    /// device -> pause_until day the caller asked to be served back.
    pub pause: HashMap<String, String>,
    /// How many POSTs the server has seen — replay counting for tests.
    pub posts: u64,
}

pub struct Running {
    url: String,
    state: Arc<Mutex<RefServer>>,
}

impl Running {
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn state(&self) -> std::sync::MutexGuard<'_, RefServer> {
        self.state.lock().expect("server")
    }
}

/// Start the reference server on an ephemeral loopback port.
pub fn start() -> Running {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let url = format!("http://{}", listener.local_addr().expect("addr"));
    let state = Arc::new(Mutex::new(RefServer::default()));
    let shared = Arc::clone(&state);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            if let Ok(stream) = stream {
                let shared = Arc::clone(&shared);
                std::thread::spawn(move || handle(stream, shared));
            }
        }
    });
    Running { url, state }
}

fn handle(mut stream: std::net::TcpStream, state: Arc<Mutex<RefServer>>) {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    // Read until headers+body complete per Content-Length.
    let (signature, body) = loop {
        let Ok(n) = stream.read(&mut chunk) else {
            return;
        };
        if n == 0 {
            return;
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some((sig, body)) = split(&buf) {
            break (sig, body);
        }
        if buf.len() > 4 * 1024 * 1024 {
            return;
        }
    };

    let answer = state.lock().map(|mut s| s.answer(&signature, &body));
    let body = match &answer {
        Ok(a) => a.clone(),
        Err(_) => b"{}".to_vec(),
    };
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.write_all(&body);
}

fn split(buf: &[u8]) -> Option<(String, Vec<u8>)> {
    let text = String::from_utf8_lossy(buf);
    let head_end = text.find("\r\n\r\n")?;
    let head = &text[..head_end];
    let len = head
        .lines()
        .find_map(|l| l.strip_prefix("Content-Length: "))
        .and_then(|v| v.trim().parse::<usize>().ok())?;
    let signature = head
        .lines()
        .find_map(|l| l.strip_prefix("Conbus-Signature: "))
        .map(|v| v.trim().to_string())?;
    let body = buf[head_end + 4..head_end + 4 + len].to_vec();
    Some((signature, body))
}

impl RefServer {
    /// Every POST: verify the signature over the body bytes against the
    /// device's key, then apply the body by its kind.
    fn answer(&mut self, signature: &str, body: &[u8]) -> Vec<u8> {
        self.posts += 1;
        let value: serde_json::Value = match serde_json::from_slice(body) {
            Ok(v) => v,
            Err(_) => return br#"{"error":"unreadable body"}"#.to_vec(),
        };

        // join registers a key; it is the one request not verified against a
        // known key (the signature proves possession of the new key's secret).
        if let (Some(invite), Some(key)) = (
            value.get("invite").and_then(serde_json::Value::as_str),
            value.get("public_key").and_then(serde_json::Value::as_str),
        ) {
            if !self.invites.contains_key(invite) {
                return br#"{"error":"unknown invite"}"#.to_vec();
            }
            if !verify(key, signature, body) {
                return br#"{"error":"bad signature"}"#.to_vec();
            }
            let device = format!("dev-{:04}", self.devices.len() + 1);
            self.devices.insert(key.to_string(), device.clone());
            return serde_json::to_vec(&serde_json::json!({ "device": device }))
                .unwrap_or_default();
        }

        // Everything else is a signed device request: find the device by its
        // key, check revocation, verify the signature, then the body's rule.
        let Some(key) = value
            .get("public_key")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                self.devices
                    .iter()
                    .find(|(_, d)| Some(d.as_str()) == value.get("device").and_then(|v| v.as_str()))
                    .map(|(k, _)| k.clone())
            })
        else {
            return br#"{"error":"unknown device"}"#.to_vec();
        };
        let Some(device) = self.devices.get(&key).cloned() else {
            return br#"{"error":"unknown device"}"#.to_vec();
        };
        if self.revoked.contains_key(&device) {
            return br#"{"revoked":true}"#.to_vec();
        }
        if !verify(&key, signature, body) {
            return br#"{"error":"bad signature"}"#.to_vec();
        }

        if value.get("leave").and_then(serde_json::Value::as_bool) == Some(true) {
            self.devices.retain(|_, d| *d != device);
            self.revisions.remove(&device);
            return br#"{"left":true}"#.to_vec();
        }
        if let Some(until) = self.pause.get(&device) {
            return serde_json::to_vec(&serde_json::json!({ "pause_until": until }))
                .unwrap_or_default();
        }
        // Batch: revision only ever goes up per device.
        let day = value
            .get("day")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default()
            .to_string();
        let revision = value
            .get("revision")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        match self.revisions.get(&device) {
            Some((seen_day, seen)) if day == *seen_day && revision <= *seen => {
                return br#"{"error":"replayed or old batch"}"#.to_vec();
            }
            _ => {}
        }
        self.revisions.insert(device, (day, revision));
        serde_json::to_vec(&serde_json::json!({ "accepted": revision })).unwrap_or_default()
    }
}

/// Ed25519 over the exact body bytes — the server's whole point.
fn verify(public_key_hex: &str, signature_hex: &str, body: &[u8]) -> bool {
    use ed25519_dalek::Verifier;
    let Ok(key_bytes) = hex::decode(public_key_hex) else {
        return false;
    };
    let Ok(sig_bytes) = hex::decode(signature_hex) else {
        return false;
    };
    let Ok(key) = ed25519_dalek::VerifyingKey::try_from(key_bytes.as_slice()) else {
        return false;
    };
    let Ok(signature) = ed25519_dalek::Signature::try_from(sig_bytes.as_slice()) else {
        return false;
    };
    key.verify(body, &signature).is_ok()
}

mod hex {
    pub fn decode(text: &str) -> Result<Vec<u8>, ()> {
        (0..text.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(text.get(i..i + 2).ok_or(())?, 16).map_err(|_| ()))
            .collect()
    }
}

// TLS: `conbus`'s transport pins `https://`, so exercising this server
// through real `/usr/bin/curl` needs a TLS listener — rustls as a
// dev-dependency + rcgen-issued throwaway CA, with `CURL_CA_BUNDLE` pointing
// curl at it (an env var, never a trust-store write). Not landed: the
// in-test transport sends the same wire bytes over plain HTTP.
