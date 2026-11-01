//! `conbus progress`: the ledger's rows, regrouped by the folder the work was in.
//!
//! `conbus usage` answers "what did each agent do"; this answers "what
//! happened in each checkout, and which agents took part". The rows are the
//! same counts — no prompt, no reply, no tool body — and the one thing added
//! is the folder's own record of progress: how many commits its `HEAD` gained
//! in the window, asked of `git` with the bound every other `git` call has.

use std::collections::{BTreeMap, HashMap};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::Serialize;

use conbus_core::paths::Paths;
use conbus_core::segment::SegmentLog;
use conbus_core::usage::{Attendance, Counts, Ledger, Row};

use crate::code;
use crate::usage::Window;

/// How many folders the text report prints before it says how many it left out.
const SHOWN: usize = 8;

/// What a conversation reported no folder for is still work, and is shown.
const NO_FOLDER: &str = "(no folder)";

/// One agent's share of one folder.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Agent {
    pub harness: String,
    pub active_days: u64,
    pub last_day: String,
    /// Turns a person was there for; the rest were a program or unknown.
    pub attended_turns: u64,
    #[serde(flatten)]
    pub counts: Counts,
}

/// One folder across the window.
#[derive(Clone, PartialEq, Eq, Debug, Serialize)]
pub struct Place {
    pub workspace: Option<String>,
    pub first_day: String,
    pub last_day: String,
    pub active_days: u64,
    /// Most attended turns first.
    pub agents: Vec<Agent>,
    /// The folders the agents reported that this place stands for: one,
    /// or a repository's checkout and its worktrees.
    pub folders: u64,
    /// Commits `HEAD` gained in the window. None when the folder is not a
    /// repository this machine can read, or `git` did not answer in time.
    pub commits: Option<u64>,
}

impl Place {
    pub fn attended_turns(&self) -> u64 {
        self.agents.iter().map(|a| a.attended_turns).sum()
    }

    pub fn turns(&self) -> u64 {
        self.agents.iter().map(|a| a.counts.turns).sum()
    }

    /// Hops and briefings: the times conbus carried one agent's work to another.
    pub fn handovers(&self) -> (u64, u64) {
        self.agents.iter().fold((0, 0), |(hops, briefings), a| {
            (hops + a.counts.hops, briefings + a.counts.briefings)
        })
    }
}

/// The rows, grouped by folder. A folder `repos` names a repository for is
/// grouped under that repository, so a checkout and its worktrees are one
/// place. `within` keeps a folder and what is under it, and the worktrees of a
/// repository that is. Busiest first: attended turns, then all turns, then by
/// name, with work that reported no folder last.
pub fn places(rows: &[Row], within: Option<&Path>, repos: &HashMap<String, String>) -> Vec<Place> {
    let mut grouped: BTreeMap<Option<&str>, Group> = BTreeMap::new();
    for row in rows {
        let folder = row.workspace.as_deref();
        let key = folder.map(|f| repos.get(f).map_or(f, String::as_str));
        if !is_within(folder, within) && !is_within(key, within) {
            continue;
        }
        let group = grouped.entry(key).or_default();
        group.folders.insert(folder);
        group.agents.entry(&row.harness).or_default().push(row);
    }
    let mut found: Vec<Place> = grouped
        .into_iter()
        .map(|(workspace, group)| place(workspace, group))
        .collect();
    // Stable over the map's name order, so ties print by name.
    found
        .sort_by_key(|p| std::cmp::Reverse((p.attended_turns(), p.turns(), p.workspace.is_some())));
    found
}

/// The rows one place is made of, by harness, and the folders they came from.
#[derive(Default)]
struct Group<'a> {
    folders: std::collections::BTreeSet<Option<&'a str>>,
    agents: BTreeMap<&'a str, Vec<&'a Row>>,
}

/// The folder `--workspace` names, written the way an agent would have
/// reported it: absolute, with `.` and `..` worked out on the text. A relative
/// one is taken from `$PWD` when that names the current directory, because an
/// agent started in a symlinked checkout reports the path it was started in,
/// and the current directory is the path with the links resolved.
pub fn folder_named(arg: &str, cwd: &Path, pwd: Option<&Path>) -> std::path::PathBuf {
    let arg = Path::new(arg);
    let same_dir = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    };
    let base = match pwd {
        Some(pwd) if pwd.is_absolute() && same_dir(pwd, cwd) => pwd,
        _ => cwd,
    };
    let mut folder = std::path::PathBuf::new();
    for part in base.join(arg).components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                folder.pop();
            }
            other => folder.push(other),
        }
    }
    folder
}

fn is_within(folder: Option<&str>, within: Option<&Path>) -> bool {
    match (within, folder) {
        (None, _) => true,
        (Some(dir), Some(folder)) => Path::new(folder).starts_with(dir),
        (Some(_), None) => false,
    }
}

fn place(workspace: Option<&str>, group: Group) -> Place {
    let Group { folders, agents } = group;
    let days = |rows: &mut dyn Iterator<Item = &Row>| -> Vec<String> {
        let mut days: Vec<String> = rows.map(|row| row.day.clone()).collect();
        days.sort();
        days.dedup();
        days
    };
    let all_days = days(&mut agents.values().flatten().copied());
    let mut agents: Vec<Agent> = agents
        .into_iter()
        .map(|(harness, rows)| agent(harness, &rows, &days(&mut rows.iter().copied())))
        .collect();
    agents.sort_by_key(|a| std::cmp::Reverse((a.attended_turns, a.counts.turns)));
    Place {
        workspace: workspace.map(str::to_string),
        first_day: all_days.first().cloned().unwrap_or_default(),
        last_day: all_days.last().cloned().unwrap_or_default(),
        active_days: all_days.len() as u64,
        agents,
        folders: folders.len() as u64,
        commits: None,
    }
}

fn agent(harness: &str, rows: &[&Row], days: &[String]) -> Agent {
    let mut counts = Counts::default();
    let mut attended_turns = 0;
    for row in rows {
        counts.add(&row.counts);
        if row.attendance == Attendance::Attended {
            attended_turns += row.counts.turns;
        }
    }
    Agent {
        harness: harness.to_string(),
        active_days: days.len() as u64,
        last_day: days.last().cloned().unwrap_or_default(),
        attended_turns,
        counts,
    }
}

pub fn run(paths: &Paths, window: Window, within: Option<&Path>, json: bool) -> io::Result<()> {
    let scan = SegmentLog::scan_meta(paths.log_dir(), Some(&paths.cache_dir()))?;
    let ledger = Ledger::fold_between(&scan.records, &window.since, window.until.as_deref());
    let budget = Budget::starting_now();
    let repos = repositories(&ledger.rows, &budget);
    let mut found = places(&ledger.rows, within, &repos);
    let shown = if json { found.len() } else { SHOWN };
    for place in found.iter_mut().take(shown) {
        let (Some(dir), Some(limit)) = (place.workspace.as_deref(), budget.left()) else {
            continue;
        };
        place.commits = code::commits(dir, &window.since, window.until.as_deref(), limit);
    }
    if budget.left().is_none() {
        eprintln!(
            "git: stopped asking after {}s; folders not placed by then stand alone, without commits",
            GIT_BUDGET.as_secs()
        );
    }
    let lines = if json {
        found
            .iter()
            .map(|place| serde_json::to_string(place).map_err(io::Error::other))
            .collect::<io::Result<Vec<_>>>()?
    } else {
        render(&found, &window.label, within)
    };
    for line in lines {
        println!("{line}");
    }
    if !scan.defects.is_empty() {
        eprintln!(
            "unreadable: {} line(s) not counted (conbus log names them)",
            scan.defects.len()
        );
    }
    Ok(())
}

/// How long one report may spend asking `git`, over every folder. A folder
/// on a volume that is gone costs a call its whole bound, and a log holds
/// every folder any agent ever worked in.
const GIT_BUDGET: Duration = Duration::from_secs(15);

/// What is left of `GIT_BUDGET`, each call bounded by the smaller of that and
/// `code::LIMIT`.
struct Budget(Instant);

impl Budget {
    fn starting_now() -> Self {
        Budget(Instant::now() + GIT_BUDGET)
    }

    fn left(&self) -> Option<Duration> {
        let left = self.0.checked_duration_since(Instant::now())?;
        (!left.is_zero()).then(|| left.min(code::LIMIT))
    }
}

/// The repository each folder belongs to, asked of `git` once per folder,
/// busiest folder first so that a budget run out leaves the quiet ones. A
/// folder `git` cannot place is left out and stands for itself.
fn repositories(rows: &[Row], budget: &Budget) -> HashMap<String, String> {
    let mut turns: HashMap<&str, u64> = HashMap::new();
    for row in rows {
        if let Some(folder) = row.workspace.as_deref() {
            *turns.entry(folder).or_default() += row.counts.turns;
        }
    }
    let mut folders: Vec<(&str, u64)> = turns.into_iter().collect();
    folders.sort_by_key(|(folder, turns)| (std::cmp::Reverse(*turns), *folder));
    let mut repos = HashMap::new();
    for (folder, _) in folders {
        let Some(limit) = budget.left() else { break };
        if let Some(repo) = code::repository(folder, limit) {
            repos.insert(folder.to_string(), repo);
        }
    }
    repos
}

fn render(places: &[Place], label: &str, within: Option<&Path>) -> Vec<String> {
    let mut lines = vec![format!(
        "progress  {label} (UTC) · {} folder(s), busiest first by turns a person was there for",
        places.len()
    )];
    if places.is_empty() {
        let under = within.map_or_else(String::new, |dir| format!(" under {}", dir.display()));
        lines.push(format!("          nothing recorded{under} in this window"));
    }
    lines.extend(places.iter().take(SHOWN).flat_map(describe));
    if places.len() > SHOWN {
        lines.push(format!(
            "          {} more folder(s) not shown; --workspace DIR shows one",
            places.len() - SHOWN
        ));
    }
    lines
}

fn describe(place: &Place) -> Vec<String> {
    let commits = place
        .commits
        .map_or_else(String::new, |n| format!(" · {n} commit(s) on HEAD"));
    let folders = if place.folders > 1 {
        format!(" ({} folders)", place.folders)
    } else {
        String::new()
    };
    let mut lines = vec![format!(
        "folder    {}{folders} — {} agent(s) · {} active day(s), last {}{commits}",
        place.workspace.as_deref().unwrap_or(NO_FOLDER),
        place.agents.len(),
        place.active_days,
        place.last_day
    )];
    let width = place
        .agents
        .iter()
        .map(|a| a.harness.len())
        .max()
        .unwrap_or(0);
    lines.extend(place.agents.iter().map(|agent| {
        let c = &agent.counts;
        format!(
            "          {:<width$}  {} turn(s), {} attended · {} day(s), last {} · {} tool call(s), {} failed · limit stops {}",
            agent.harness,
            c.turns,
            agent.attended_turns,
            agent.active_days,
            agent.last_day,
            c.tool_calls,
            c.tool_failures,
            c.limit_stops
        )
    }));
    let (hops, briefings) = place.handovers();
    if place.agents.len() > 1 || hops > 0 {
        lines.push(format!(
            "          handed over: {hops} hop(s) · {briefings} briefing(s)"
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    static NO_REPOS: std::sync::LazyLock<HashMap<String, String>> =
        std::sync::LazyLock::new(HashMap::new);

    fn row(day: &str, harness: &str, workspace: Option<&str>, attendance: Attendance) -> Row {
        Row {
            day: day.into(),
            harness: harness.into(),
            workspace: workspace.map(str::to_string),
            attendance,
            conversations: 1,
            counts: Counts {
                turns: 2,
                tool_calls: 10,
                tool_failures: 1,
                ..Counts::default()
            },
        }
    }

    #[test]
    fn rows_of_one_folder_become_one_place_with_one_line_per_agent() {
        let rows = [
            row(
                "2026-10-01",
                "claude_code",
                Some("/w/a"),
                Attendance::Attended,
            ),
            row(
                "2026-10-01",
                "claude_code",
                Some("/w/a"),
                Attendance::Unattended,
            ),
            row(
                "2026-10-03",
                "claude_code",
                Some("/w/a"),
                Attendance::Attended,
            ),
            row("2026-10-02", "codex", Some("/w/a"), Attendance::Unknown),
        ];
        let found = places(&rows, None, &NO_REPOS);
        assert_eq!(found.len(), 1);
        let place = &found[0];
        assert_eq!(place.workspace.as_deref(), Some("/w/a"));
        assert_eq!(
            (place.first_day.as_str(), place.last_day.as_str()),
            ("2026-10-01", "2026-10-03")
        );
        assert_eq!(place.active_days, 3);
        assert_eq!(place.turns(), 8);
        assert_eq!(place.attended_turns(), 4);
        let claude = &place.agents[0];
        assert_eq!(claude.harness, "claude_code");
        // Two rows on one day are one active day.
        assert_eq!(claude.active_days, 2);
        assert_eq!(claude.last_day, "2026-10-03");
        assert_eq!(claude.attended_turns, 4);
        assert_eq!(claude.counts.turns, 6);
        assert_eq!(claude.counts.tool_calls, 30);
        assert_eq!(place.agents[1].harness, "codex");
        assert_eq!(place.agents[1].attended_turns, 0);
    }

    #[test]
    fn a_folder_a_person_worked_in_outranks_one_a_program_was_busier_in() {
        let mut busy = row(
            "2026-10-01",
            "claude_code",
            Some("/batch"),
            Attendance::Unattended,
        );
        busy.counts.turns = 500;
        let rows = [
            busy,
            row("2026-10-01", "codex", Some("/repo"), Attendance::Attended),
            row("2026-10-01", "grok", None, Attendance::Unknown),
        ];
        let order: Vec<_> = places(&rows, None, &NO_REPOS)
            .into_iter()
            .map(|p| p.workspace)
            .collect();
        assert_eq!(
            order,
            [Some("/repo".to_string()), Some("/batch".to_string()), None]
        );
    }

    #[test]
    fn within_keeps_the_folder_and_what_is_under_it_and_nothing_beside_it() {
        let rows = [
            row("2026-10-01", "codex", Some("/w/a"), Attendance::Attended),
            row(
                "2026-10-01",
                "codex",
                Some("/w/a/sub"),
                Attendance::Attended,
            ),
            row("2026-10-01", "codex", Some("/w/ab"), Attendance::Attended),
            row("2026-10-01", "codex", None, Attendance::Attended),
        ];
        let mut kept: Vec<_> = places(&rows, Some(Path::new("/w/a")), &NO_REPOS)
            .into_iter()
            .filter_map(|p| p.workspace)
            .collect();
        kept.sort();
        assert_eq!(kept, ["/w/a", "/w/a/sub"]);
    }

    #[test]
    fn the_worktrees_of_one_repository_are_one_place_that_says_how_many_folders() {
        let rows = [
            row(
                "2026-10-01",
                "claude_code",
                Some("/r/main"),
                Attendance::Attended,
            ),
            row(
                "2026-10-02",
                "claude_code",
                Some("/r/main/.claude/worktrees/x"),
                Attendance::Attended,
            ),
            row(
                "2026-10-02",
                "codex",
                Some("/elsewhere/wt"),
                Attendance::Attended,
            ),
            row("2026-10-02", "codex", Some("/plain"), Attendance::Attended),
        ];
        let repos: HashMap<String, String> = [
            ("/r/main", "/r/main"),
            ("/r/main/.claude/worktrees/x", "/r/main"),
            ("/elsewhere/wt", "/r/main"),
        ]
        .into_iter()
        .map(|(a, b)| (a.to_string(), b.to_string()))
        .collect();
        let found = places(&rows, None, &repos);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].workspace.as_deref(), Some("/r/main"));
        assert_eq!(found[0].folders, 3);
        assert_eq!(found[0].turns(), 6);
        assert_eq!(found[0].active_days, 2);
        assert_eq!(found[1].workspace.as_deref(), Some("/plain"));
        assert_eq!(found[1].folders, 1);
    }

    #[test]
    fn within_a_repository_keeps_its_worktrees_wherever_they_are() {
        let rows = [
            row(
                "2026-10-01",
                "claude_code",
                Some("/r/main"),
                Attendance::Attended,
            ),
            row(
                "2026-10-01",
                "codex",
                Some("/elsewhere/wt"),
                Attendance::Attended,
            ),
            row("2026-10-01", "codex", Some("/other"), Attendance::Attended),
        ];
        let repos: HashMap<String, String> = [("/elsewhere/wt", "/r/main")]
            .into_iter()
            .map(|(a, b)| (a.to_string(), b.to_string()))
            .collect();
        let found = places(&rows, Some(Path::new("/r/main")), &repos);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].folders, 2);
        assert_eq!(found[0].agents.len(), 2);
    }

    #[test]
    fn work_that_reported_no_folder_comes_after_a_folder_as_busy() {
        let rows = [
            row("2026-10-01", "grok", None, Attendance::Attended),
            row("2026-10-01", "codex", Some("/w"), Attendance::Attended),
        ];
        let order: Vec<_> = places(&rows, None, &NO_REPOS)
            .into_iter()
            .map(|p| p.workspace)
            .collect();
        assert_eq!(order, [Some("/w".to_string()), None]);
    }

    #[test]
    fn a_folder_named_with_dots_is_the_folder_they_lead_to() {
        let cwd = Path::new("/w/repo/sub");
        assert_eq!(folder_named(".", cwd, None), Path::new("/w/repo/sub"));
        assert_eq!(
            folder_named("../other", cwd, None),
            Path::new("/w/repo/other")
        );
        assert_eq!(folder_named("/a/./b/../c", cwd, None), Path::new("/a/c"));
        assert_eq!(folder_named("../../../..", cwd, None), Path::new("/"));
    }

    #[test]
    fn a_relative_folder_is_taken_from_the_shell_path_when_it_is_this_directory() {
        let real = tempfile::tempdir().expect("dir");
        let real = real.path().canonicalize().expect("path");
        let links = tempfile::tempdir().expect("dir");
        let link = links.path().join("checkout");
        std::os::unix::fs::symlink(&real, &link).expect("symlink");
        // As the agent was started: in the link.
        assert_eq!(folder_named(".", &real, Some(&link)), link);
        // A $PWD left over from somewhere else is not trusted.
        assert_eq!(folder_named(".", &real, Some(links.path())), real);
        assert_eq!(folder_named(".", &real, Some(Path::new("relative"))), real);
    }

    #[test]
    fn handovers_add_the_hops_and_briefings_of_every_agent() {
        let mut from = row(
            "2026-10-01",
            "claude_code",
            Some("/w"),
            Attendance::Attended,
        );
        from.counts.hops = 1;
        from.counts.briefings = 2;
        let mut to = row("2026-10-01", "codex", Some("/w"), Attendance::Attended);
        to.counts.briefings = 1;
        assert_eq!(places(&[from, to], None, &NO_REPOS)[0].handovers(), (1, 3));
    }

    fn place(workspace: Option<&str>, agents: usize, commits: Option<u64>) -> Place {
        let names = ["claude_code", "codex", "cursor"];
        Place {
            workspace: workspace.map(str::to_string),
            first_day: "2026-10-01".into(),
            last_day: "2026-10-03".into(),
            active_days: 2,
            agents: names[..agents]
                .iter()
                .map(|harness| Agent {
                    harness: (*harness).into(),
                    active_days: 2,
                    last_day: "2026-10-03".into(),
                    attended_turns: 3,
                    counts: Counts {
                        turns: 4,
                        tool_calls: 1200,
                        tool_failures: 7,
                        limit_stops: 1,
                        ..Counts::default()
                    },
                })
                .collect(),
            folders: 1,
            commits,
        }
    }

    #[test]
    fn the_text_names_each_folder_its_agents_its_commits_and_its_handovers() {
        let text = render(
            &[place(Some("/w/repo"), 2, Some(14)), place(None, 1, None)],
            "from 2026-10-01",
            None,
        )
        .join("\n");
        assert!(
            text.starts_with("progress  from 2026-10-01 (UTC) · 2 folder(s)"),
            "{text}"
        );
        assert!(text.contains("folder    /w/repo — 2 agent(s) · 2 active day(s), last 2026-10-03 · 14 commit(s) on HEAD"), "{text}");
        assert!(text.contains("          claude_code  4 turn(s), 3 attended · 2 day(s), last 2026-10-03 · 1200 tool call(s), 7 failed · limit stops 1"), "{text}");
        assert!(
            text.contains("          handed over: 0 hop(s) · 0 briefing(s)"),
            "{text}"
        );
        assert!(
            text.contains("folder    (no folder) — 1 agent(s)"),
            "{text}"
        );
        // A folder with no repository says nothing about commits rather than zero.
        assert!(
            !text.contains("(no folder) — 1 agent(s) · 2 active day(s), last 2026-10-03 ·"),
            "{text}"
        );
        // One agent alone has nobody to hand over to.
        assert_eq!(text.matches("handed over").count(), 1, "{text}");
    }

    #[test]
    fn a_place_made_of_several_folders_says_so() {
        let mut merged = place(Some("/r/main"), 1, Some(2));
        merged.folders = 3;
        let text = render(&[merged], "from 2026-10-01", None).join("\n");
        assert!(
            text.contains("folder    /r/main (3 folders) — 1 agent(s)"),
            "{text}"
        );
    }

    #[test]
    fn the_text_says_how_many_folders_it_left_out() {
        let many: Vec<_> = (0..SHOWN + 3).map(|_| place(Some("/w"), 1, None)).collect();
        let text = render(&many, "from 2026-10-01", None).join("\n");
        assert_eq!(text.matches("\nfolder    ").count(), SHOWN, "{text}");
        assert!(
            text.ends_with("          3 more folder(s) not shown; --workspace DIR shows one"),
            "{text}"
        );
    }

    #[test]
    fn an_empty_window_says_so_and_names_the_folder_it_looked_in() {
        let text = render(&[], "from 2026-10-01", Some(Path::new("/w/a"))).join("\n");
        assert!(
            text.contains("nothing recorded under /w/a in this window"),
            "{text}"
        );
    }
}
