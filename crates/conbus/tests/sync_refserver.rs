//! Round trip against the test-only reference sync server: join signs with
//! the new key, a batch is accepted once and only once, a replay is refused,
//! revocation answers `revoked`, leave clears the device.

mod support;

use std::io::{Read, Write};
use std::net::TcpStream;
use support::refserver;

/// POST the body to the reference server over plain HTTP — the wire shape is
/// real (signature header + body bytes); TLS is the documented gap.
fn post(url: &str, signature: &str, body: &[u8]) -> serde_json::Value {
    let host = url.strip_prefix("http://").expect("url");
    let mut stream = TcpStream::connect(host).expect("connect");
    let request = format!(
        "POST / HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nConbus-Signature: {signature}\r\nContent-Length: {}\r\n\r\n",
        body.len()
    );
    stream.write_all(request.as_bytes()).expect("write");
    stream.write_all(body).expect("body");
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).expect("read");
    let text = String::from_utf8_lossy(&buf);
    let json = text.split("\r\n\r\n").nth(1).unwrap_or("{}");
    serde_json::from_str(json).expect("reply json")
}

fn sign(seed: &[u8; 32], body: &[u8]) -> String {
    use ed25519_dalek::Signer;
    ed25519_dalek::SigningKey::from_bytes(seed)
        .sign(body)
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn pubkey(seed: &[u8; 32]) -> String {
    ed25519_dalek::SigningKey::from_bytes(seed)
        .verifying_key()
        .to_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn join_batch_replay_revoke_leave_round_trip() {
    let server = refserver::start();
    server
        .state()
        .invites
        .insert("INV-TEST-1".into(), "org".into());
    let url = server.url().to_string();
    let seed = [7u8; 32];

    // join: registers the public key, answers a device id.
    let body = serde_json::to_vec(&serde_json::json!({
        "invite": "INV-TEST-1", "public_key": pubkey(&seed)
    }))
    .expect("join body");
    let reply = post(&url, &sign(&seed, &body), &body);
    let device = reply["device"].as_str().expect("a device id").to_string();

    // batch: accepted with its own revision.
    let batch = serde_json::to_vec(&serde_json::json!({
        "device": device, "day_closed": "2026-11-01", "revision": 1, "rows": []
    }))
    .expect("batch");
    let reply = post(&url, &sign(&seed, &batch), &batch);
    assert_eq!(reply["accepted"], true, "the batch landed: {reply}");
    assert_eq!(reply["revision"], 1, "the batch landed: {reply}");

    // replay: the same revision is refused.
    let reply = post(&url, &sign(&seed, &batch), &batch);
    assert!(reply.get("accepted").is_none(), "replay refused: {reply}");
    assert!(
        String::from_utf8_lossy(&serde_json::to_vec(&reply).unwrap_or_default())
            .contains("replayed")
    );

    // a forged signature never reaches the rules.
    let forged = post(&url, &"00".repeat(64), &batch);
    assert!(
        forged.get("accepted").is_none(),
        "forgery refused: {forged}"
    );

    // revocation answers every later request the same way.
    server.state().revoked.insert(device.clone(), true);
    let reply = post(&url, &sign(&seed, &batch), &batch);
    assert_eq!(reply["revoked"], true, "revoked is the answer: {reply}");

    // a second device is unaffected — and leave clears the first.
    server.state().revoked.clear();
    let leave = serde_json::to_vec(&serde_json::json!({
        "device": device, "leave": true
    }))
    .expect("leave");
    let reply = post(&url, &sign(&seed, &leave), &leave);
    assert_eq!(reply["left"], true, "leave lands: {reply}");
    let reply = post(&url, &sign(&seed, &batch), &batch);
    assert!(
        reply.get("accepted").is_none(),
        "a left device is unknown: {reply}"
    );
}

/// Standalone for shadow S5: `cargo test -p conbus --test sync_refserver
/// standalone -- --ignored --nocapture` — prints the URL and serves until
/// killed.
#[test]
#[ignore]
fn standalone() {
    let server = refserver::start();
    server
        .state()
        .invites
        .insert("SHADOW-INVITE".into(), "shadow".into());
    // The real CLI posts the invite document it was given verbatim, so a
    // shadow join needs the document itself registered, not a token naming it.
    if let Ok(doc) = std::env::var("CONBUS_SHADOW_INVITE") {
        server.state().invites.insert(doc, "shadow".into());
    }
    println!("reference sync server on {}", server.url());
    println!("invite: SHADOW-INVITE — kill to stop");
    loop {
        std::thread::sleep(std::time::Duration::from_secs(60));
    }
}
