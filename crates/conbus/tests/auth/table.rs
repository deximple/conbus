//! The vendor table: what needs a sign-in, and what conbus runs for each.

use super::*;

#[test]
fn auth_table_names_what_needs_a_login_and_exit_stays_zero() {
    let (_agy_dir, agy, calls) = fake_logged_out_agy();
    let (_gh_dir, gh) = fake_gh(0);
    let root = tempfile::tempdir().expect("tempdir");
    let output = conbus(root.path())
        .arg("auth")
        .env("CONBUS_AGY_BIN", &agy)
        .env("CONBUS_GH", &gh)
        .output()
        .expect("run auth");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(
        output.status.success(),
        "a vendor without login must not fail the command: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        stdout.contains("needs no login of its own"),
        "the conbus line: {stdout}"
    );
    assert!(stdout.contains("AUTH_REQUIRED"), "agy's line: {stdout}");
    assert!(
        stdout.contains("gh") && stdout.contains("signed in"),
        "gh's line: {stdout}"
    );
    // `auth` may check the login question but never `-p`, which is what
    // opened the browser tabs that made this command necessary.
    let called = std::fs::read_to_string(&calls).unwrap_or_default();
    assert!(
        !called.lines().any(|line| line == "-p"),
        "auth must never run `agy -p`: {called}"
    );
}

#[test]
fn the_background_row_names_what_runs_with_nobody_at_the_keyboard() {
    // "Nothing conbus runs on its own asks a vendor to sign in" had no support:
    // gap fill and a hop start vendor programs, and a hop opens vendor apps.
    let table = the_table();

    assert!(!table.contains("nothing conbus runs on its own"), "{table}");
    let background = row(&table, "background");
    for word in [
        "`codex app-server`",
        "gap fill",
        "`thread/start`",
        "`claude -p`",
        "`gh`",
        "`open -b`",
        "`cursor -n`",
        "`osascript`",
    ] {
        assert!(background.contains(word), "{word} in {background}");
    }
    let measured = row(&table, "measured");
    for word in [
        "Codex 0.166",
        "`thread/list`",
        "`thread/start`",
        "unmeasured",
    ] {
        assert!(measured.contains(word), "{word} in {measured}");
    }
}

#[test]
fn a_vendor_a_hop_can_open_is_not_called_unused() {
    let table = the_table();

    assert!(!table.contains("not used"), "{table}");
    for (name, opener) in [
        ("cursor", "`cursor -n`"),
        ("grok", "`osascript`"),
        ("opencode", "`osascript`"),
    ] {
        let line = row(&table, name);
        assert!(line.contains("not checked"), "{line}");
        assert!(
            line.contains("hop") && line.contains(opener) && line.contains("`conbus run`"),
            "{line}"
        );
    }
    assert!(row(&table, "opencode").contains("`opencode --version`"));
    let claude = row(&table, "claude");
    for word in ["`claude -p`", "`summary`", "`conbus run`"] {
        assert!(claude.contains(word), "{word} in {claude}");
    }
}

#[test]
fn the_agy_row_names_everything_that_runs_agy() {
    let table = the_table();

    let agy = row(&table, "agy");
    for word in ["`conbus quota`", "`conbus auth`", "hop", "`conbus run`"] {
        assert!(agy.contains(word), "{word} in {agy}");
    }
}
