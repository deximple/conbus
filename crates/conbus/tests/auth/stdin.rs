//! `--url -`: the address from stdin, and the warning when it is not.

use super::*;

/// `conbus auth --url -` with `input` on stdin.
fn ask_on_stdin(input: &[u8]) -> std::process::Output {
    use std::io::Write;
    let root = tempfile::tempdir().expect("tempdir");
    let mut child = conbus(root.path())
        .args(["auth", "--url", "-"])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(input)
        .expect("write");
    child.wait_with_output().expect("wait")
}

#[test]
fn a_url_on_stdin_gets_the_same_answer_and_no_warning() {
    // `pbpaste | conbus auth --url -`: the token reaches no shell history and no
    // recorded tool call.
    let output = ask_on_stdin(format!("{BRIDGE_WITH_TOKEN}\n").as_bytes());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    assert!(output.status.success(), "{stderr}");
    assert!(stdout.contains("Playwright MCP Bridge"), "{stdout}");
    assert!(
        !stdout.contains("TOK3N") && !stderr.contains("TOK3N"),
        "{stdout} {stderr}"
    );
    assert_eq!(stderr, "", "no warning: the URL is not on the command line");
}

#[test]
fn stdin_gives_the_first_line_that_is_not_blank() {
    let output =
        ask_on_stdin(b"\n  \nhttps://auth.openai.com/authorize\nhttps://github.com/login\n");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(stdout.contains("OpenAI sign-in"), "{stdout}");
    assert!(!stdout.contains("github.com"), "{stdout}");
}

#[test]
fn stdin_with_no_url_on_it_fails_and_says_so() {
    for input in [&b""[..], b"\n\n", b"   \n"] {
        let output = ask_on_stdin(input);
        assert_eq!(output.status.code(), Some(1), "{input:?}");
        assert_eq!(String::from_utf8_lossy(&output.stdout), "", "{input:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("no URL on stdin"),
            "{input:?}"
        );
    }
}

#[test]
fn bytes_on_stdin_that_are_not_text_are_not_a_crash() {
    let output = ask_on_stdin(b"https://x.example/?a=\xff\xfe\n");

    assert!(output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("unknown page"));
}

#[test]
fn a_query_on_the_command_line_is_warned_about() {
    let root = tempfile::tempdir().expect("tempdir");
    for url in [
        "https://x.example/?token=SECRET-VALUE",
        "https://x.example/#token=SECRET-VALUE",
    ] {
        let output = conbus(root.path())
            .args(["auth", "--url", url])
            .output()
            .expect("run auth --url");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(output.status.success());
        assert!(stderr.contains("shell history"), "{url}: {stderr}");
        assert!(stderr.contains("conbus auth --url -"), "{url}: {stderr}");
        assert!(!stderr.contains("SECRET-VALUE"), "{stderr}");
        assert!(
            String::from_utf8_lossy(&output.stdout).contains("unknown page"),
            "it still answers"
        );
    }

    let output = conbus(root.path())
        .args(["auth", "--url", "https://x.example/"])
        .output()
        .expect("run auth --url");
    assert_eq!(
        String::from_utf8_lossy(&output.stderr),
        "",
        "nothing to warn of"
    );
}
