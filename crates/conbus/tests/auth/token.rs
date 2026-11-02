//! A token in the address is not looked for, printed or compared with anything.

use super::*;

/// The vendor files the public docs promise conbus does not read for a token.
const VENDOR_CONFIGS: &[&str] = &[
    ".codex/config.toml",
    ".cursor/mcp.json",
    ".claude.json",
    ".config/opencode/opencode.json",
];

/// Run to the end or fail: what this guards against is a hang.
fn finishes(mut command: Command, what: &str) {
    let mut child = command
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("spawn");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while child.try_wait().expect("wait").is_none() {
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{what} tried to open a vendor config file");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

#[test]
fn no_form_of_the_command_opens_a_vendor_config_file() {
    // Each file is a FIFO, and opening a FIFO to read it blocks until something
    // writes. So this catches a read whatever becomes of its result, which no
    // look at the output could.
    let home = tempfile::tempdir().expect("tempdir");
    for config in VENDOR_CONFIGS {
        let path = home.path().join(config);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        let made = Command::new("mkfifo").arg(&path).status().expect("mkfifo");
        assert!(made.success(), "mkfifo {config}");
    }
    let page = home.path().join("auth.html");

    let mut url = conbus(home.path());
    url.args(["auth", "--url", BRIDGE_WITH_TOKEN]);
    finishes(url, "auth --url");

    let mut table = conbus(home.path());
    table.arg("auth");
    finishes(table, "auth");

    let mut html = conbus(home.path());
    html.args(["auth", "--html", page.to_str().expect("utf8")]);
    finishes(html, "auth --html");
}

#[test]
fn a_token_in_the_url_is_not_looked_for_and_not_printed() {
    let home = tempfile::tempdir().expect("tempdir");
    let codex = home.path().join(".codex");
    std::fs::create_dir_all(&codex).expect("mkdir");
    std::fs::write(
        codex.join("config.toml"),
        "mcp_servers.playwright.token = \"TOK3N-USED-FOR-TEST\"\n",
    )
    .expect("config");

    let output = conbus(home.path())
        .args(["auth", "--url", BRIDGE_WITH_TOKEN])
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(!stdout.contains("TOK3N"), "{stdout}");
    assert!(!stdout.lines().any(|l| l.starts_with("token")), "{stdout}");
    assert!(!stdout.contains("came from"), "{stdout}");
    assert!(!stdout.contains("config.toml"), "{stdout}");
}

#[test]
fn the_cause_is_a_condition_because_nothing_here_saw_the_page() {
    // The cause used to be stated for every bridge address, a token or not.
    for url in [
        "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html",
        BRIDGE_WITH_TOKEN,
    ] {
        let stdout = ask(url);
        let cause = row(&stdout, "cause");
        assert!(
            cause.contains("if the page says `Invalid token provided`"),
            "{stdout}"
        );
        assert!(!cause.contains("that file"), "{stdout}");
        let fix = row(&stdout, "fix");
        assert!(fix.contains("`PLAYWRIGHT_MCP_EXTENSION_TOKEN`"), "{stdout}");
        assert!(
            fix.contains("`PLAYWRIGHT_MCP_PROFILE_DIR_NAME`"),
            "{stdout}"
        );
    }
}

#[test]
fn only_the_bridge_gets_a_cause_and_a_fix() {
    for url in [
        "https://accounts.google.com/o/oauth2/auth?token=x",
        "https://evil.example/?x=chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/",
    ] {
        let stdout = ask(url);
        assert!(
            !stdout
                .lines()
                .any(|l| l.starts_with("cause") || l.starts_with("fix")),
            "{url}: {stdout}"
        );
    }
}
