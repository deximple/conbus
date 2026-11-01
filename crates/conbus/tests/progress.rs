//! End-to-end: `conbus progress`, from a real log on disk and a real
//! repository with a worktree, through the real binary.

use std::path::Path;
use std::process::{Command, Output};

use conbus_core::event::{Body, Entrypoint, Event, Harness, Source, ToolOutcome, TurnState};
use conbus_core::record::Entry;
use conbus_core::retention::Retention;
use conbus_core::segment::SegmentLog;

const DAY: &str = "2026-10-01";

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_conbus")
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .env("GIT_AUTHOR_DATE", format!("{DAY}T12:00:00Z"))
        .env("GIT_COMMITTER_DATE", format!("{DAY}T12:00:00Z"))
        .output()
        .expect("git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// A repository with two commits on `DAY`, and a worktree added from it.
fn checkout_with_a_worktree(root: &Path) -> (String, String) {
    let main = root.join("repo");
    std::fs::create_dir(&main).expect("repo");
    git(&main, &["init", "-q"]);
    for message in ["one", "two"] {
        git(
            &main,
            &[
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "--allow-empty",
                "-m",
                message,
            ],
        );
    }
    let worktree = root.join("wt");
    git(
        &main,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            worktree.to_str().expect("utf-8"),
        ],
    );
    let canonical = |p: &Path| {
        p.canonicalize()
            .expect("path")
            .to_str()
            .expect("utf-8")
            .to_string()
    };
    (canonical(&main), canonical(&worktree))
}

/// Claude Code in the checkout, Codex in its worktree. Every body is a canary.
fn two_agents_in_one_repository(home: &Path, main: &str, worktree: &str) {
    let mut log = SegmentLog::open(home.join("log"), Retention::Full).expect("log");
    let source = |harness: &str, workspace: &str| Source {
        harness: Harness::new(harness),
        harness_version: None,
        vendor_session_id: None,
        workspace: Some(workspace.into()),
        entrypoint: Some(Entrypoint::new("cli")),
        backfilled_from: None,
        profile: None,
    };
    let legs = [
        (
            "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3e",
            Harness::CLAUDE_CODE,
            main,
        ),
        (
            "01996b1e-2c4a-7c31-9d0e-4f2a6b8c1d3f",
            Harness::CODEX,
            worktree,
        ),
    ];
    for (minute, (bus_id, harness, workspace)) in legs.into_iter().enumerate() {
        let events = [
            Event::TurnStarted {
                author: None,
                prompt: Body::kept("the secret plan"),
            },
            Event::ToolUse {
                tool: "Bash".into(),
                tool_use_id: None,
                duration_ms: Some(40),
                input: Body::kept("cargo test"),
                outcome: ToolOutcome::Failed {
                    error: Body::kept("password=hunter2"),
                    interrupted: false,
                },
            },
            Event::TurnEnded {
                state: TurnState::Complete,
                assistant_message: Some(Body::kept("the secret reply")),
                tokens: None,
            },
        ];
        for (second, event) in events.into_iter().enumerate() {
            log.append(Entry {
                bus_id: bus_id.parse().expect("id"),
                observed_at: Some(
                    format!("{DAY}T09:0{minute}:0{second}.000Z")
                        .parse()
                        .expect("ts"),
                ),
                source: source(harness, workspace),
                vendor_turn_id: None,
                vendor_agent_id: None,
                hook_id: None,
                spooled_at: None,
                event,
            })
            .expect("append");
        }
    }
}

fn progress(home: &Path, args: &[&str]) -> Output {
    Command::new(bin())
        .arg("progress")
        .args(args)
        .env("CONBUS_HOME", home)
        .output()
        .expect("run conbus progress")
}

#[test]
fn a_checkout_and_its_worktree_are_one_place_with_both_agents_and_the_commits() {
    let root = tempfile::tempdir().expect("root");
    let home = root.path().join("home");
    let (main, worktree) = checkout_with_a_worktree(root.path());
    two_agents_in_one_repository(&home, &main, &worktree);

    let out = progress(&home, &["--since", DAY, "--json"]);
    assert!(out.status.success(), "{out:?}");
    let stdout = String::from_utf8(out.stdout).expect("utf-8");
    let lines: Vec<&str> = stdout.lines().collect();
    assert_eq!(lines.len(), 1, "{stdout}");
    let place: serde_json::Value = serde_json::from_str(lines[0]).expect("json");
    assert_eq!(place["workspace"], main.as_str());
    assert_eq!(place["folders"], 2);
    assert_eq!(place["commits"], 2);
    let agents: Vec<&str> = place["agents"]
        .as_array()
        .expect("agents")
        .iter()
        .map(|a| a["harness"].as_str().expect("harness"))
        .collect();
    assert_eq!(agents, ["claude_code", "codex"]);
    assert_eq!(place["agents"][1]["tool_failures"], 1);
    for canary in ["secret", "hunter2", "cargo test"] {
        assert!(
            !stdout.contains(canary),
            "a body reached the report: {stdout}"
        );
    }

    let text = progress(&home, &["--since", DAY, "--workspace", &worktree]);
    let text = String::from_utf8(text.stdout).expect("utf-8");
    assert!(
        text.contains(&format!("folder    {main} — 1 agent(s)")),
        "--workspace keeps the worktree, under its checkout: {text}"
    );
    assert!(text.contains("codex"), "{text}");
    assert!(!text.contains("claude_code"), "{text}");
}
