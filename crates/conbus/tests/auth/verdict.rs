//! `--url`: the walk behind `yes`, `no` and `cannot tell`.

use super::*;

/// Stand-ins for `lsof` and `ps` that answer from a table, so a verdict can be
/// pinned without the machine's real processes. `lsof` also keeps its calls.
struct ProcessTable {
    dir: tempfile::TempDir,
    lsof_calls: std::path::PathBuf,
}

impl ProcessTable {
    /// `listeners` hold every port asked about. `processes` is pid, ppid, name;
    /// a pid not in it is one `ps` no longer finds. Each leads its own process
    /// group, as anything launchd or systemd started does.
    fn new(listeners: &[u32], processes: &[(u32, u32, &str)]) -> Self {
        Self::with_groups(listeners, processes, &[])
    }

    /// As [`ProcessTable::new`], with the group of each pid in `groups` named.
    fn with_groups(
        listeners: &[u32],
        processes: &[(u32, u32, &str)],
        groups: &[(u32, u32)],
    ) -> Self {
        Self::build(listeners, processes, groups, &[])
    }

    /// As [`ProcessTable::new`], with the command line of each pid in `commands`
    /// named. Any other reads as `<name> quota`.
    fn with_commands(
        listeners: &[u32],
        processes: &[(u32, u32, &str)],
        commands: &[(u32, &str)],
    ) -> Self {
        Self::build(listeners, processes, &[], commands)
    }

    fn build(
        listeners: &[u32],
        processes: &[(u32, u32, &str)],
        groups: &[(u32, u32)],
        commands: &[(u32, &str)],
    ) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let lsof_calls = dir.path().join("lsof.calls");
        let pids: String = listeners.iter().map(|pid| format!("{pid}\n")).collect();
        let found = if listeners.is_empty() { 1 } else { 0 };
        support::write_executable(
            &dir.path().join("lsof"),
            &format!(
                "#!/bin/sh\n[ \"$1\" = --warm ] && exit 0\nprintf '%s\\n' \"$*\" >>'{}'\nprintf '%s' '{pids}'\nexit {found}\n",
                lsof_calls.display()
            ),
        );
        let arms: String = processes
            .iter()
            .map(|(pid, ppid, name)| {
                let group = groups
                    .iter()
                    .find(|(member, _)| member == pid)
                    .map_or(*pid, |(_, group)| *group);
                format!("  {pid}) echo '{ppid} {group} {name}' ;;\n")
            })
            .collect();
        let command_arms: String = processes
            .iter()
            .map(|(pid, _, name)| {
                let command = commands
                    .iter()
                    .find(|(member, _)| member == pid)
                    .map_or(format!("{name} quota"), |(_, command)| command.to_string());
                format!("  {pid}) echo '{command}' ;;\n")
            })
            .collect();
        support::write_executable(
            &dir.path().join("ps"),
            &format!(
                "#!/bin/sh\nwant=$2\nwhile [ $# -gt 1 ]; do shift; done\nif [ \"$want\" = args= ]; then\ncase $1 in\n{command_arms}  *) exit 1 ;;\nesac\nexit 0\nfi\ncase $1 in\n{arms}  *) exit 1 ;;\nesac\n"
            ),
        );
        warmed(&dir.path().join("lsof"));
        warmed(&dir.path().join("ps"));
        ProcessTable { dir, lsof_calls }
    }

    /// `conbus auth --url` with these stand-ins first on PATH.
    fn ask(&self, url: &str) -> String {
        let root = tempfile::tempdir().expect("tempdir");
        let path = format!(
            "{}:{}",
            self.dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        let output = conbus(root.path())
            .args(["auth", "--url", url])
            .env("PATH", path)
            .output()
            .expect("run auth --url");
        assert!(
            output.status.success(),
            "{url}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    fn lsof_calls(&self) -> String {
        std::fs::read_to_string(&self.lsof_calls).unwrap_or_default()
    }
}

/// The address the bridge opens: a relay on a loopback port.
fn bridge_url(port: u16) -> String {
    format!(
        "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html?mcpRelayUrl=ws%3A%2F%2F127.0.0.1%3A{port}%2F"
    )
}

/// What a walked-up listener under Codex.app looks like.
const CODEX_TREE: &[(u32, u32, &str)] = &[
    (4242, 4000, "/opt/node/bin/node"),
    (4000, 3000, "codex"),
    (3000, 1, "/Applications/Codex.app/Contents/MacOS/Codex"),
    (1, 0, "/sbin/launchd"),
];

#[test]
fn no_takes_a_listener_walked_all_the_way_to_the_root() {
    let table = ProcessTable::new(&[4242], CODEX_TREE);

    let stdout = table.ask(&bridge_url(55407));

    assert!(verdict_of(&stdout).starts_with("conbus    no:"), "{stdout}");
    assert!(
        stdout.contains("process   port 55407: pid 4242: node > codex > Codex > launchd"),
        "the walk it rests on, names only: {stdout}"
    );
    assert!(table.lsof_calls().contains("-iTCP:55407"), "{stdout}");
}

#[test]
fn no_says_what_a_walk_cannot_see() {
    // An app `open` started hangs from launchd like any other, so an ancestry says
    // nothing about it. And every conbus read starts `codex app-server`, so "starts
    // no server" was false as written.
    let table = ProcessTable::new(&[4242], CODEX_TREE);

    let stdout = table.ask(&bridge_url(55407));

    assert!(!stdout.contains("starts no server"), "{stdout}");
    let limit = stdout
        .lines()
        .find(|line| line.starts_with("limit "))
        .unwrap_or_else(|| panic!("no limit line: {stdout}"));
    for word in [
        "`open`",
        "`osascript`",
        "`codex app-server`",
        "`conbus status`",
    ] {
        assert!(limit.contains(word), "{word} in {limit}");
    }
    let measured = stdout
        .lines()
        .find(|line| line.starts_with("measured "))
        .unwrap_or_else(|| panic!("no measured line: {stdout}"));
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
fn conbus_far_above_the_listener_is_still_yes() {
    // The walk used to stop at eight levels, without a word, and say `no`.
    let names: Vec<String> = (0..11).map(|level| format!("level{level}")).collect();
    let mut tree: Vec<(u32, u32, &str)> = names
        .iter()
        .enumerate()
        .map(|(level, name)| (100 + level as u32, 101 + level as u32, name.as_str()))
        .collect();
    tree.push((111, 112, "/somewhere/bin/conbus"));
    tree.push((112, 1, "launcher"));
    tree.push((1, 0, "/sbin/launchd"));
    let table = ProcessTable::new(&[100], &tree);

    let stdout = table.ask(&bridge_url(55407));

    assert!(
        verdict_of(&stdout).starts_with("conbus    yes:"),
        "{stdout}"
    );
    assert!(
        stdout.contains("level10 > conbus > launcher > launchd"),
        "{stdout}"
    );
}

#[test]
fn every_listener_on_the_port_is_walked() {
    // Two listeners on one port, the conbus one second: only the first used to
    // be walked, and the answer was `no`.
    let table = ProcessTable::new(
        &[4242, 777],
        &[
            (4242, 1, "/opt/node/bin/node"),
            (777, 776, "python3"),
            (776, 1, "conbus"),
            (1, 0, "/sbin/launchd"),
        ],
    );

    let stdout = table.ask(&bridge_url(55407));

    assert!(
        verdict_of(&stdout).starts_with("conbus    yes:"),
        "{stdout}"
    );
    assert!(stdout.contains("pid 4242: node > launchd"), "{stdout}");
    assert!(
        stdout.contains("pid 777: python3 > conbus > launchd"),
        "{stdout}"
    );
}

#[test]
fn only_the_name_of_the_binary_makes_a_process_conbus() {
    for (name, yes) in [
        ("conbus", true),
        ("/home/someone/.local/bin/conbus", true),
        ("conbus.next", true),
        ("conbus-scratch", true),
        ("notconbus", false),
        ("conbusd", false),
        ("/home/someone/conbus/bin/node", false),
        ("/tmp/conbus-work/node", false),
    ] {
        let table = ProcessTable::new(
            &[50],
            &[
                (50, 55, "node"),
                (55, 60, name),
                (60, 1, "/Applications/Some.app/Contents/MacOS/Some"),
                (1, 0, "/sbin/launchd"),
            ],
        );
        let stdout = table.ask(&bridge_url(55407));
        let want = if yes {
            "conbus    yes:"
        } else {
            "conbus    no:"
        };
        assert!(verdict_of(&stdout).starts_with(want), "{name}: {stdout}");
    }
}

#[test]
fn a_callback_on_another_host_is_not_looked_up_here() {
    // The port of `another.example` was answered from this machine's process
    // table, whose listener on that number has nothing to do with it.
    let table = ProcessTable::new(&[4242], CODEX_TREE);

    for url in [
        "https://evil.example/login?redirect_uri=https%3A%2F%2Fanother.example%3A55407%2Fcb",
        "https://evil.example/?redirect_uri=http%3A%2F%2F127.0.0.1.another.example%3A55407%2F",
        "https://evil.example/?redirect_uri=http%3A%2F%2Flocalhost.another.example%3A55407%2F",
        "https://evil.example/?redirect_uri=http%3A%2F%2F10.0.0.1%3A55407%2F",
        "https://evil.example/?redirect_uri=http%3A%2F%2F%5B2001%3Adb8%3A%3A1%5D%3A55407%2F",
    ] {
        let stdout = table.ask(url);
        assert!(
            verdict_of(&stdout).starts_with("conbus    cannot tell:"),
            "{url}: {stdout}"
        );
        assert!(stdout.contains("another host"), "{url}: {stdout}");
        assert!(
            !stdout.contains("another.example") && !stdout.contains("10.0.0.1"),
            "a query value is not printed: {stdout}"
        );
    }
    assert_eq!(table.lsof_calls(), "", "no remote port goes to lsof");
}

#[test]
fn a_callback_on_this_machine_is_found_however_it_is_written() {
    for (value, port) in [
        ("http%3A%2F%2F127.0.0.1%3A55407%2Fcb", 55407),
        ("http%3A%2F%2F127.0.0.2%3A55407%2Fcb", 55407),
        ("http%3A%2F%2Flocalhost%3A55407%2Fcb", 55407),
        ("http%3A%2F%2FLOCALHOST%3A55407", 55407),
        ("ws%3A%2F%2F127.0.0.1%3A55407", 55407),
        ("http%3A%2F%2F%5B%3A%3A1%5D%3A55407%2Fcb", 55407),
        (
            "http%3A%2F%2F%5B%3A%3Affff%3A127.0.0.1%5D%3A55407%2Fcb",
            55407,
        ),
        ("localhost%3A55407%2Fcb", 55407),
        ("127.0.0.1%3A55407", 55407),
        // The number after the userinfo's colon is a password, not a port.
        ("http%3A%2F%2Fuser%3A1234%40127.0.0.1%3A9%2Fcb", 9),
    ] {
        let table = ProcessTable::new(&[4242], CODEX_TREE);
        let stdout = table.ask(&format!(
            "https://accounts.google.com/o/oauth2/auth?redirect_uri={value}"
        ));
        assert!(
            table.lsof_calls().contains(&format!("-iTCP:{port} ")),
            "{value} should ask about {port}: {}",
            table.lsof_calls()
        );
        assert!(
            verdict_of(&stdout).starts_with("conbus    no:"),
            "{value}: {stdout}"
        );
    }
}

#[test]
fn with_nothing_to_walk_it_cannot_tell_and_says_what_would() {
    let empty = ProcessTable::new(&[], &[]);

    // No listener on the port.
    let stdout = empty.ask(&bridge_url(55407));
    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
    assert!(stdout.contains("still waits"), "what would tell: {stdout}");

    // No port in the URL at all, and a callback with none.
    for url in [
        "https://accounts.google.com/o/oauth2/auth?client_id=x",
        "https://accounts.google.com/o/oauth2/auth?redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb",
        "https://accounts.google.com/o/oauth2/auth?callback=someFunction",
        "https://accounts.google.com/o/oauth2/auth?redirect_uri=urn%3Aietf%3Awg%3Aoauth%3A2.0%3Aoob",
    ] {
        let stdout = empty.ask(url);
        assert!(
            verdict_of(&stdout).starts_with("conbus    cannot tell:"),
            "{url}: {stdout}"
        );
        assert!(stdout.contains("would tell"), "{url}: {stdout}");
    }
    assert_eq!(
        empty.lsof_calls().lines().count(),
        1,
        "only the first asked"
    );
}

#[test]
fn with_no_lsof_it_cannot_tell() {
    let root = tempfile::tempdir().expect("tempdir");
    let empty = tempfile::tempdir().expect("tempdir");

    let output = conbus(root.path())
        .args(["auth", "--url", &bridge_url(55407)])
        .env("PATH", empty.path())
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);

    assert!(output.status.success());
    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
    assert!(stdout.contains("`lsof`"), "what would tell: {stdout}");
}

#[test]
fn a_walk_that_cannot_reach_the_root_is_not_a_no() {
    // A parent `ps` no longer finds, a process that is its own ancestor, and
    // two that are each other's.
    for tree in [
        &[(50, 40, "node")][..],
        &[(50, 50, "node")][..],
        &[(50, 40, "node"), (40, 50, "npm")][..],
    ] {
        let table = ProcessTable::new(&[50], tree);
        let stdout = table.ask(&bridge_url(55407));
        assert!(
            verdict_of(&stdout).starts_with("conbus    cannot tell:"),
            "{tree:?}: {stdout}"
        );
        assert!(stdout.contains("before the root"), "{tree:?}: {stdout}");
    }
}

#[test]
fn a_listener_that_outlived_what_started_it_is_not_a_no() {
    // Directly under the root and not the leader of its own process group: what
    // started it has exited, and with it the ancestry. A read's server left behind
    // by a conbus that has gone looks exactly like this.
    let table = ProcessTable::with_groups(
        &[50],
        &[(50, 1, "python3"), (1, 0, "/sbin/launchd")],
        &[(50, 40)],
    );

    let stdout = table.ask(&bridge_url(55407));

    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
    assert!(stdout.contains("may have exited"), "{stdout}");
    assert!(
        stdout.contains("`conbus status`"),
        "what would tell: {stdout}"
    );
    assert!(stdout.contains("`ps -o lstart= -p 50`"), "{stdout}");
    assert!(stdout.contains("pid 50: python3 > launchd"), "{stdout}");
}

#[test]
fn a_helper_of_a_live_parent_needs_no_group_of_its_own() {
    // Only a process directly under the root says anything by its group.
    let table = ProcessTable::with_groups(
        &[50],
        &[
            (50, 40, "node"),
            (40, 30, "npm"),
            (30, 1, "/Applications/Codex.app/Contents/MacOS/Codex"),
            (1, 0, "/sbin/launchd"),
        ],
        &[(50, 30), (40, 30)],
    );

    let stdout = table.ask(&bridge_url(55407));

    assert!(verdict_of(&stdout).starts_with("conbus    no:"), "{stdout}");
}

#[test]
fn conbus_above_a_listener_outranks_a_broken_group() {
    let table = ProcessTable::with_groups(
        &[50, 60],
        &[
            (50, 1, "python3"),
            (60, 61, "node"),
            (61, 1, "conbus"),
            (1, 0, "/sbin/launchd"),
        ],
        &[(50, 40)],
    );

    let stdout = table.ask(&bridge_url(55407));

    assert!(
        verdict_of(&stdout).starts_with("conbus    yes:"),
        "{stdout}"
    );
}

#[test]
fn a_port_with_several_callbacks_needs_all_of_them_clear_for_no() {
    let table = ProcessTable::new(&[4242], CODEX_TREE);
    let local = "http%3A%2F%2F127.0.0.1%3A55407%2F";
    let remote = "https%3A%2F%2Fanother.example%3A1%2F";

    let stdout = table.ask(&format!(
        "https://x.example/?redirect_uri={local}&callback={remote}"
    ));

    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
}

#[test]
fn conbus_above_one_callback_is_yes_whatever_another_says() {
    let table = ProcessTable::new(
        &[50],
        &[(50, 40, "node"), (40, 1, "conbus"), (1, 0, "launchd")],
    );
    let local = "http%3A%2F%2F127.0.0.1%3A55407%2F";
    let remote = "https%3A%2F%2Fanother.example%3A1%2F";

    let stdout = table.ask(&format!(
        "https://accounts.google.com/o/oauth2/auth?redirect_uri={remote}&callback={local}"
    ));

    assert!(
        verdict_of(&stdout).starts_with("conbus    yes:"),
        "{stdout}"
    );
}

/// The real `lsof` and `ps`, for what stand-ins cannot pin: the arguments.
fn have_lsof_and_ps() -> bool {
    ["lsof", "ps"].iter().all(|name| {
        Command::new("sh")
            .args(["-c", &format!("command -v {name}")])
            .output()
            .is_ok_and(|output| output.status.success())
    })
}

/// A stand-in `codex app-server` that listens on a port of its own and stays
/// quiet until told to stop, so a conbus read is still waiting on it.
const LISTENING_CODEX: &str = r#"#!/usr/bin/env python3
import os, socket, sys, time
here = os.path.dirname(os.path.abspath(__file__))
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(1)
with open(os.path.join(here, "port.tmp"), "w") as f:
    f.write(str(s.getsockname()[1]))
os.rename(os.path.join(here, "port.tmp"), os.path.join(here, "port"))
deadline = time.time() + 30
while time.time() < deadline and not os.path.exists(os.path.join(here, "stop")):
    time.sleep(0.05)
"#;

#[test]
fn a_real_listener_under_a_real_conbus_is_yes() {
    if !have_lsof_and_ps() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let codex_dir = tempfile::tempdir().expect("tempdir");
    let codex = codex_dir.path().join("codex");
    support::write_executable(&codex, LISTENING_CODEX);
    let port_file = codex_dir.path().join("port");
    let mut quota = conbus(root.path())
        .arg("quota")
        .env("CONBUS_CODEX_BIN", &codex)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run quota");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let port = loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            break text.trim().parse::<u16>().expect("port");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the stand-in never listened"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    let output = conbus(root.path())
        .args(["auth", "--url", &bridge_url(port)])
        .output()
        .expect("run auth --url");
    std::fs::write(codex_dir.path().join("stop"), "").expect("stop");
    quota.wait().expect("quota ends with its server");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        verdict_of(&stdout).starts_with("conbus    yes:"),
        "{stdout}"
    );
    assert!(stdout.contains(" > conbus > "), "{stdout}");
}

/// A stand-in `codex app-server` that starts a server of its own and exits, leaving
/// it behind: what a read looks like when the vendor's server outlives it.
const CODEX_LEAVING_A_SERVER: &str = r#"#!/usr/bin/env python3
import os, socket, subprocess, sys
here = os.path.dirname(os.path.abspath(__file__))
server = """
import os, socket, sys, time
here = sys.argv[1]
s = socket.socket()
s.bind(("127.0.0.1", 0))
s.listen(1)
with open(os.path.join(here, "port.tmp"), "w") as f:
    f.write(str(s.getsockname()[1]))
os.rename(os.path.join(here, "port.tmp"), os.path.join(here, "port"))
deadline = time.time() + 30
while time.time() < deadline and not os.path.exists(os.path.join(here, "stop")):
    time.sleep(0.05)
"""
subprocess.Popen([sys.executable, "-c", server, here], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
"#;

#[test]
fn a_real_server_left_behind_by_a_read_is_not_a_no() {
    if !have_lsof_and_ps() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let codex_dir = tempfile::tempdir().expect("tempdir");
    let codex = codex_dir.path().join("codex");
    support::write_executable(&codex, CODEX_LEAVING_A_SERVER);
    let quota = conbus(root.path())
        .arg("quota")
        .env("CONBUS_CODEX_BIN", &codex)
        .output()
        .expect("run quota");
    assert!(quota.status.code().is_some());
    let port_file = codex_dir.path().join("port");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let port = loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            break text.trim().parse::<u16>().expect("port");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the stand-in's server never listened"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    let output = conbus(root.path())
        .args(["auth", "--url", &bridge_url(port)])
        .output()
        .expect("run auth --url");
    std::fs::write(codex_dir.path().join("stop"), "").expect("stop");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let said = verdict_of(&stdout);
    assert!(!said.starts_with("conbus    yes:"), "{stdout}");
    // macOS hands an orphan to launchd. Linux can hand it to a subreaper, whose
    // own parent keeps the chain whole; there the limit line has to say it.
    if cfg!(target_os = "macos") {
        assert!(said.starts_with("conbus    cannot tell:"), "{stdout}");
    } else {
        assert!(
            said.starts_with("conbus    cannot tell:") || stdout.contains("limit "),
            "{stdout}"
        );
    }
}

#[test]
fn a_real_listener_is_walked_to_the_root() {
    if !have_lsof_and_ps() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();

    let output = conbus(root.path())
        .args(["auth", "--url", &bridge_url(port)])
        .output()
        .expect("run auth --url");
    let stdout = String::from_utf8_lossy(&output.stdout);

    // This test's own process holds the port, so the line starts with it. The verdict
    // depends on what the suite runs under: `yes` under `conbus run`, and `cannot tell`
    // on a machine whose top process does not lead its own group.
    let me = std::env::current_exe().expect("exe");
    let me = me.file_name().expect("name").to_string_lossy();
    let shown: String = me.chars().take(15).collect();
    let process = stdout
        .lines()
        .find(|line| line.starts_with("process "))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(process.contains(&format!(": {shown}")), "{stdout}");
    assert!(!stdout.contains("nothing lsof sees"), "{stdout}");
}

#[test]
fn a_server_in_its_own_group_left_behind_by_codex_is_not_a_no() {
    // codex-rs/rmcp-client starts stdio MCP servers with ProcessMode::NewGroup.
    let table = ProcessTable::new(
        &[4242],
        &[(4242, 1, "/opt/node/bin/node"), (1, 0, "/sbin/launchd")],
    );
    let stdout = table.ask(&bridge_url(55407));
    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
}

#[test]
fn an_orphan_adopted_by_a_user_service_manager_is_not_a_no() {
    let table = ProcessTable::with_groups(
        &[5000],
        &[
            (5000, 1200, "node"),
            (1200, 1, "systemd"),
            (1, 0, "systemd"),
        ],
        &[(5000, 4000)],
    );
    let stdout = table.ask(&bridge_url(55407));
    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
}

#[test]
fn lsof_is_asked_for_listeners_only() {
    // guard: passes at head
    let table = ProcessTable::new(&[4242], CODEX_TREE);
    table.ask(&bridge_url(55407));
    assert!(
        table.lsof_calls().contains("-sTCP:LISTEN"),
        "{}",
        table.lsof_calls()
    );
}

#[test]
fn a_listener_hanging_from_a_manager_reads_cannot_tell_even_as_a_group_leader() {
    // Whatever it leads, nothing above it says who started it.
    for tree in [
        &[(50, 1, "/sbin/launchd-child"), (1, 0, "/sbin/launchd")][..],
        &[
            (50, 1, "/Applications/Some.app/Contents/MacOS/Some"),
            (1, 0, "/sbin/launchd"),
        ][..],
        &[(50, 1, "server"), (1, 0, "/sbin/init")][..],
        &[
            (50, 1200, "server"),
            (1200, 1, "systemd"),
            (1, 0, "systemd"),
        ][..],
        &[(1, 0, "/sbin/launchd")][..],
    ] {
        let listener = tree[0].0;
        let table = ProcessTable::new(&[listener], tree);
        let stdout = table.ask(&bridge_url(55407));
        assert!(
            verdict_of(&stdout).starts_with("conbus    cannot tell:"),
            "{tree:?}: {stdout}"
        );
    }
}

#[test]
fn a_chain_whose_top_nothing_vouches_for_is_not_a_no() {
    // The real shape of a server Codex left behind: the listener has a parent, and
    // that parent is what lost its own. npm leads its group, as Codex starts it.
    for tree in [
        &[(50, 40, "node"), (40, 1, "npm"), (1, 0, "/sbin/launchd")][..],
        &[
            (50, 40, "node"),
            (40, 30, "sh"),
            (30, 1, "npm"),
            (1, 0, "/sbin/launchd"),
        ][..],
        &[
            (50, 40, "node"),
            (40, 1200, "npm"),
            (1200, 1, "systemd"),
            (1, 0, "systemd"),
        ][..],
        // A command-line tool inside an app bundle is not the app.
        &[
            (50, 40, "node"),
            (40, 1, "/Applications/Codex.app/Contents/Resources/codex"),
            (1, 0, "/sbin/launchd"),
        ][..],
        // Python's own framework build runs from a bundle of that name.
        &[
            (50, 40, "node"),
            (
                40,
                1,
                "/opt/Python.framework/Resources/Python.app/Contents/MacOS/Python",
            ),
            (1, 0, "/sbin/launchd"),
        ][..],
    ] {
        let table = ProcessTable::new(&[50], tree);
        let stdout = table.ask(&bridge_url(55407));
        assert!(
            verdict_of(&stdout).starts_with("conbus    cannot tell:"),
            "{tree:?}: {stdout}"
        );
        assert!(
            stdout.contains("`conbus status`"),
            "what would tell: {stdout}"
        );
    }
}

#[test]
fn a_chain_up_to_a_person_s_app_or_session_can_be_a_no() {
    for tree in [
        CODEX_TREE,
        &[
            (50, 40, "node"),
            (40, 30, "-zsh"),
            (30, 20, "login"),
            (
                20,
                1,
                "/System/Applications/Utilities/Terminal.app/Contents/MacOS/Terminal",
            ),
            (1, 0, "/sbin/launchd"),
        ][..],
        &[
            (50, 40, "node"),
            (40, 30, "bash"),
            (30, 1, "sshd"),
            (1, 0, "systemd"),
        ][..],
        &[
            (50, 40, "node"),
            (40, 30, "bash"),
            (30, 1200, "tmux: server"),
            (1200, 1, "systemd"),
            (1, 0, "systemd"),
        ][..],
    ] {
        let listener = tree[0].0;
        let table = ProcessTable::new(&[listener], tree);
        let stdout = table.ask(&bridge_url(55407));
        assert!(
            verdict_of(&stdout).starts_with("conbus    no:"),
            "{tree:?}: {stdout}"
        );
    }
}

#[test]
fn an_app_that_lost_its_own_parent_is_not_a_no() {
    // A helper inside a bundle, daemonised away from its app: the group gives it away.
    let table = ProcessTable::with_groups(
        &[50],
        &[
            (50, 40, "node"),
            (40, 1, "/Applications/Some.app/Contents/MacOS/Some"),
            (1, 0, "/sbin/launchd"),
        ],
        &[(40, 39)],
    );
    let stdout = table.ask(&bridge_url(55407));
    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "{stdout}"
    );
}

#[test]
fn a_page_nobody_vouches_for_does_not_earn_a_yes_from_a_port_it_named() {
    let table = ProcessTable::new(
        &[50],
        &[
            (50, 40, "node"),
            (40, 30, "claude"),
            (30, 1, "conbus"),
            (1, 0, "/sbin/launchd"),
        ],
    );
    let stdout =
        table.ask("https://evil.example/login?redirect_uri=http%3A%2F%2F127.0.0.1%3A3000%2F");
    assert!(stdout.starts_with("this      unknown page"), "{stdout}");
    assert!(
        !verdict_of(&stdout).starts_with("conbus    yes:"),
        "{stdout}"
    );
}

/// What listens hangs from a conbus command, which hangs from an app.
const UNDER_A_CONBUS_READ: &[(u32, u32, &str)] = &[
    (50, 40, "node"),
    (40, 30, "codex"),
    (30, 20, "conbus"),
    (20, 1, "/Applications/Some.app/Contents/MacOS/Some"),
    (1, 0, "/sbin/launchd"),
];

/// A page with its callback on `port`.
fn page_calling_back(page: &str, port: u16) -> String {
    format!("{page}?redirect_uri=http%3A%2F%2F127.0.0.1%3A{port}%2F")
}

#[test]
fn a_vendor_conbus_runs_under_a_conbus_command_is_yes_and_names_the_command() {
    for page in [
        "https://accounts.google.com/o/oauth2/auth",
        "https://auth.openai.com/oauth/authorize",
        "https://console.anthropic.com/",
        "https://claude.ai/login",
        "chrome-extension://mmlmfjhmonkocbjadbfplnigmagldckm/connect.html",
    ] {
        let table = ProcessTable::new(&[50], UNDER_A_CONBUS_READ);
        let stdout = table.ask(&page_calling_back(page, 55407));
        let said = verdict_of(&stdout);
        assert!(said.starts_with("conbus    yes:"), "{page}: {stdout}");
        assert!(said.contains("`conbus quota`"), "{page}: {stdout}");
    }
}

#[test]
fn a_page_for_a_vendor_conbus_does_not_run_that_way_is_not_a_yes() {
    // conbus runs `gh` for a lookup, and opens Cursor and Grok without being their
    // parent. None of those can be what asked through a process under conbus.
    for page in [
        "https://github.com/login/oauth/authorize",
        "https://cursor.com/loginDeepControl",
        "https://accounts.x.ai/sign-in",
        "https://evil.example/login",
    ] {
        let table = ProcessTable::new(&[50], UNDER_A_CONBUS_READ);
        let stdout = table.ask(&page_calling_back(page, 55407));
        let said = verdict_of(&stdout);
        assert!(
            said.starts_with("conbus    cannot tell:"),
            "{page}: {stdout}"
        );
        assert!(said.contains("can name any port"), "{page}: {stdout}");
    }
}

#[test]
fn a_listener_under_conbus_run_belongs_to_the_person_s_own_agent() {
    for page in [
        "https://claude.ai/login",
        "https://accounts.google.com/o/oauth2/auth",
        "https://evil.example/login",
    ] {
        let table = ProcessTable::with_commands(
            &[50],
            &[
                (50, 40, "node"),
                (40, 30, "claude"),
                (30, 20, "conbus"),
                (20, 1, "/Applications/Some.app/Contents/MacOS/Some"),
                (1, 0, "/sbin/launchd"),
            ],
            &[(
                30,
                "/home/someone/.local/bin/conbus run claude_code --profile work",
            )],
        );
        let stdout = table.ask(&page_calling_back(page, 55407));
        let said = verdict_of(&stdout);
        assert!(
            said.starts_with("conbus    cannot tell:"),
            "{page}: {stdout}"
        );
        assert!(said.contains("`conbus run`"), "{page}: {stdout}");
        assert!(said.contains("your own agent"), "{page}: {stdout}");
        assert!(
            !stdout.contains("--profile") && !stdout.contains("work"),
            "{stdout}"
        );
    }
}

#[test]
fn conbus_run_anywhere_above_outranks_a_nearer_conbus_command() {
    let table = ProcessTable::with_commands(
        &[50],
        &[
            (50, 45, "node"),
            (45, 40, "conbus"),
            (40, 30, "claude"),
            (30, 1, "conbus"),
            (1, 0, "/sbin/launchd"),
        ],
        &[(45, "conbus quota"), (30, "conbus run claude_code")],
    );

    let stdout = table.ask(&page_calling_back("https://claude.ai/login", 55407));

    assert!(verdict_of(&stdout).contains("`conbus run`"), "{stdout}");
}

#[test]
fn conbus_s_own_listener_is_not_a_yes() {
    // `conbus hook --inference-listen` sits on a port anyone can name.
    let table = ProcessTable::with_commands(
        &[50],
        &[
            (50, 40, "conbus"),
            (40, 1, "/Applications/Some.app/Contents/MacOS/Some"),
            (1, 0, "/sbin/launchd"),
        ],
        &[(50, "conbus hook --inference-listen")],
    );

    let stdout = table.ask(&page_calling_back(
        "https://accounts.google.com/o/oauth2/auth",
        8787,
    ));

    let said = verdict_of(&stdout);
    assert!(said.starts_with("conbus    cannot tell:"), "{stdout}");
    assert!(said.contains("conbus itself listens"), "{stdout}");
}

#[test]
fn a_conbus_whose_command_cannot_be_read_is_not_a_yes() {
    for command in ["", "conbus", "conbus --odd", "/bin/sh -c conbus"] {
        let table = ProcessTable::with_commands(&[50], UNDER_A_CONBUS_READ, &[(30, command)]);
        let stdout = table.ask(&page_calling_back("https://claude.ai/login", 55407));
        let said = verdict_of(&stdout);
        assert!(
            said.starts_with("conbus    cannot tell:"),
            "{command:?}: {stdout}"
        );
        assert!(said.contains("which command"), "{command:?}: {stdout}");
    }
}

#[test]
fn a_real_listener_under_a_real_conbus_run_is_not_a_yes() {
    if !have_lsof_and_ps() {
        return;
    }
    let root = tempfile::tempdir().expect("tempdir");
    let agent_dir = tempfile::tempdir().expect("tempdir");
    let agent = agent_dir.path().join("claude");
    support::write_executable(&agent, LISTENING_CODEX);
    let mut run = conbus(root.path())
        .args(["run", "claude_code"])
        .env("CONBUS_CLAUDE_BIN", &agent)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("run conbus run");
    let port_file = agent_dir.path().join("port");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    let port = loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            break text.trim().parse::<u16>().expect("port");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the stand-in agent never listened"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };

    let output = conbus(root.path())
        .args([
            "auth",
            "--url",
            &page_calling_back("https://claude.ai/login", port),
        ])
        .output()
        .expect("run auth --url");
    std::fs::write(agent_dir.path().join("stop"), "").expect("stop");
    run.wait().expect("conbus run ends with its agent");

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(verdict_of(&stdout).contains("`conbus run`"), "{stdout}");
}

#[test]
fn nothing_that_may_be_a_second_callback_allows_a_no() {
    // One clean callback, and something else that may be where the page really goes.
    let local = "http%3A%2F%2F127.0.0.1%3A55407%2F";
    for extra in [
        "callback=http%3A%2F%2Flocalhost%3A99999%2F",
        "callback=not-an-address",
        "callback=",
        "callback",
        "redirect_url=cursor%3A%2F%2Fanysphere.cursor-retrieval%2Foauth",
        "redirectUri=https%3A%2F%2Fanother.example%3A1%2F",
        "callbackUrl=https%3A%2F%2Fanother.example%3A1%2F",
        "return_to=https%3A%2F%2Fanother.example%3A1%2F",
        "returnUrl=https%3A%2F%2Fanother.example%3A1%2F",
        "relayUrl=ws%3A%2F%2Fanother.example%3A1%2F",
        "REDIRECT-URI=https%3A%2F%2Fanother.example%3A1%2F",
        // Under a name nothing here reads, but naming this machine all the same.
        "to=http%3A%2F%2F127.0.0.1%3A2%2F",
        "next=http%3A%2F%2Flocalhost%3A2%2F",
        "go=ws%3A%2F%2F%5B%3A%3A1%5D%3A2%2F",
    ] {
        let table = ProcessTable::new(&[4242], CODEX_TREE);
        let stdout = table.ask(&format!(
            "https://accounts.google.com/o/oauth2/auth?redirect_uri={local}&{extra}"
        ));
        assert!(
            verdict_of(&stdout).starts_with("conbus    cannot tell:"),
            "{extra}: {stdout}"
        );
    }

    let table = ProcessTable::new(&[4242], CODEX_TREE);
    let stdout = table.ask(&format!(
        "https://accounts.google.com/o/oauth2/auth?redirect_uri={local}#to=http://127.0.0.1:2/"
    ));
    assert!(
        verdict_of(&stdout).starts_with("conbus    cannot tell:"),
        "a fragment: {stdout}"
    );
}

#[test]
fn a_callback_under_another_spelling_of_the_name_is_looked_up() {
    for name in [
        "redirectUri",
        "callbackUrl",
        "REDIRECT-URI",
        "returnTo",
        "relay_url",
    ] {
        let table = ProcessTable::new(&[4242], CODEX_TREE);
        let stdout = table.ask(&format!(
            "https://accounts.google.com/o/oauth2/auth?{name}=http%3A%2F%2F127.0.0.1%3A55407%2F"
        ));
        assert!(
            table.lsof_calls().contains("-iTCP:55407 "),
            "{name}: {stdout}"
        );
        assert!(
            verdict_of(&stdout).starts_with("conbus    no:"),
            "{name}: {stdout}"
        );
    }
}

#[test]
fn what_a_real_sign_in_carries_beside_its_callback_does_not_stop_a_no() {
    // A scope is a URL, and a state is noise. Neither names this machine.
    let table = ProcessTable::new(&[4242], CODEX_TREE);
    let stdout = table.ask(
        "https://accounts.google.com/o/oauth2/auth?client_id=x&scope=https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fcloud-platform&state=abc123&redirect_uri=http%3A%2F%2Flocalhost%3A55407%2Foauth2callback",
    );
    assert!(verdict_of(&stdout).starts_with("conbus    no:"), "{stdout}");
}

#[test]
fn the_real_claude_pages_can_earn_a_yes() {
    for page in [
        "https://platform.claude.com/oauth/authorize",
        "https://claude.com/cai/oauth/authorize",
        "https://antigravity.google/auth-success",
    ] {
        let table = ProcessTable::new(&[50], UNDER_A_CONBUS_READ);
        let stdout = table.ask(&page_calling_back(page, 55407));
        assert!(
            verdict_of(&stdout).starts_with("conbus    yes:"),
            "{page}: {stdout}"
        );
    }
}
