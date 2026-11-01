//! Helpers the unit tests share.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

/// Write an executable script without this process ever holding it open for
/// writing.
///
/// Linux refuses to exec a file that any process holds open for writing
/// (`ETXTBSY`), and tests run in threads of one process: a child that another
/// test thread is forking inherits this thread's write descriptor until its own
/// exec, so a stub written with `fs::write` and run straight away fails now and
/// then. A short-lived `sh` writes it instead; once it has exited, nothing holds
/// the file open.
pub fn write_executable(path: &Path, contents: &str) {
    let mut child = Command::new("/bin/sh")
        .args(["-c", "cat > \"$1\" && chmod 755 \"$1\"", "sh"])
        .arg(path)
        .stdin(Stdio::piped())
        .spawn()
        .expect("spawn sh to write a stub");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(contents.as_bytes())
        .expect("write the stub");
    let status = child.wait().expect("sh exit");
    assert!(status.success(), "could not write {}", path.display());
}
