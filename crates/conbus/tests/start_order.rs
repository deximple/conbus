//! The socket is bound only after restore: a hook that arrives while the
//! daemon is still preparing meets a refused or absent socket at once,
//! instead of waiting 500 ms on a listener that cannot answer.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

/// The stamp's `serving` field, when the stamp exists at all.
fn serving(home: &std::path::Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join("daemon.build")).ok()?;
    let stamp: serde_json::Value = serde_json::from_str(text.trim()).ok()?;
    stamp
        .get("serving")
        .and_then(|s| s.as_str())
        .map(String::from)
}

#[test]
fn the_socket_answers_only_once_the_stamp_says_serving() {
    let home = tempfile::tempdir().expect("tempdir");
    let socket = home.path().join("conbus.sock");
    // The test knob widens the prepare->bind window from microseconds to
    // something a poll can observe.
    let mut child = Command::new(bin())
        .arg("daemon")
        .env("CONBUS_HOME", home.path())
        .env("CONBUS_SKIP_LAUNCHCTL", "1")
        .env("CONBUS_TEST_PAUSE_BEFORE_BIND_MS", "800")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn daemon");

    // While the stamp says "starting" the socket must refuse — this is the
    // property: no listener accepts a hook it cannot serve.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut refused = false;
    let mut served = false;
    while Instant::now() < deadline {
        match serving(home.path()) {
            None => {}
            Some(_) => {
                served = true;
                break;
            }
        }
        if std::os::unix::net::UnixStream::connect(&socket).is_err() {
            refused = true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    if !served {
        let _ = child.kill();
        panic!("the daemon never stamped `serving`");
    }
    assert!(
        refused || serving(home.path()).is_none(),
        "the socket accepted before `serving` was stamped"
    );

    // Once the stamp says serving, the socket really answers.
    let answered = std::os::unix::net::UnixStream::connect(&socket).is_ok();
    let _ = child.kill();
    let _ = child.wait();
    assert!(answered, "a stamped-serving daemon must answer its socket");
}
