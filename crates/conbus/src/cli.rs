//! One binary, argv dispatch. Deployment is a single file, and the hook has to
//! be registered by absolute path anyway because a vendor's `PATH` is not ours.

/// How the binary was invoked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    Daemon,
    /// Read a vendor hook payload on stdin and mirror it.
    Hook {
        harness: String,
        /// Claude StatusLine stdin: persist `rate_limits`, never hook JSON.
        statusline: bool,
        /// Official Inference hooks HTTP endpoint (Anthropic POSTs here).
        inference_listen: Option<String>,
    },
    Log {
        json: bool,
        filter: LogFilter,
    },
    /// Register the hook with the harnesses. Reports by default; `--apply`
    /// writes, after backing up whatever was there.
    Install {
        apply: bool,
        /// Print the hooks block for Claude Code's managed settings instead,
        /// and write nothing. Wins over `--apply`: managed settings belong to
        /// the organisation's device tooling, never to conbus.
        managed: bool,
    },
    /// Take back everything `install --apply` wrote. Reports by default; the
    /// log is never removed.
    Uninstall {
        apply: bool,
    },
    /// Recover history from before the recorder was installed.
    ///
    /// `--gaps` is the other job: turns that happened while the daemon was
    /// down, written under the live `bus_id` rather than as a second
    /// conversation.
    Backfill {
        apply: bool,
        gaps: bool,
        /// Official Claude.ai / Desktop `conversations.json` or its zip.
        chat_export: Option<String>,
    },
    Doctor,
    /// The control plane: routing rules, quota, hop history, cost.
    ///
    /// Not a chat. The vendor TUI stays the window.
    Status,
    /// Ask Codex's app-server how much of the account's quota is used.
    Quota,
    /// Decide which harness should serve the next turn.
    Hop {
        from: Option<String>,
        to: Option<String>,
        apply: bool,
        /// Print the recorded failover. Never opens a vendor window.
        demo: bool,
    },
    /// Opposite-leg digest. Never the protected harness.
    Summary {
        bus_id: Option<String>,
    },
    /// Four-week dogfood score from the live log.
    Verdict,
    /// What the agents did, counted: conversations, turns, stops, tool
    /// failures, permission asks — per harness, from the live log.
    Usage {
        days: u32,
        since: Option<String>,
        /// One closed UTC day; wins over `days` and `since`.
        day: Option<String>,
        json: bool,
        /// Print what a sync would send (RFD 0002): the rows, with no folder.
        shareable: bool,
    },
    /// Same TTY: start a vendor, and exec the next one when a hop lands.
    Run {
        harness: String,
    },
    /// Confirm a fact or link two workspaces so the next pack can see it.
    Arena {
        action: String,
        workspace: Option<String>,
        kind: Option<String>,
        claim: Option<String>,
        anchor: Option<String>,
        from: Option<String>,
        to: Option<String>,
        bus_id: Option<String>,
        seq: Option<u64>,
    },
    /// Say which build this binary is.
    Version,
    Help,
    Unknown(String),
}

/// The harness assumed when nothing says otherwise. Stage 0 registers against
/// Claude Code first; the other harnesses that read the same settings format
/// arrive with `--harness`.
pub const DEFAULT_HARNESS: &str = conbus_core::event::Harness::CLAUDE_CODE;

/// `conbus usage` without `--days`: a week, ending today.
pub const DEFAULT_USAGE_DAYS: u32 = 7;

/// Which records `conbus log` should print.
///
/// A log that only prints everything stops being queryable the moment it holds
/// more than a screen, which is immediately after backfill.
#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct LogFilter {
    /// One conversation, by a prefix of its id.
    ///
    /// Note that `bus_id` is a v7 UUID, so its leading characters are a
    /// timestamp: conversations minted in the same moment share a prefix, and a
    /// short one will match more than one of them. Paste enough of the id.
    pub bus_id: Option<String>,
    /// Records at or after this timestamp. Compared as text, which works
    /// because the format is fixed-width — and a prefix like `2026-08-31`
    /// therefore works too.
    pub since: Option<String>,
    /// Only the last N matching records.
    pub tail: Option<usize>,
    /// A substring of the rendered line.
    pub grep: Option<String>,
    /// Decrypt sealed tool bodies. Off by default.
    pub reveal: bool,
}

pub fn parse<I, S>(args: I, env_harness: Option<String>) -> Command
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let args: Vec<String> = args.into_iter().map(|a| a.as_ref().to_string()).collect();
    let Some(first) = args.first() else {
        return Command::Help;
    };
    // `conbus go --help` must not apply a hop. A leading `-h`/`--help` after
    // the subcommand is usage, not a catalog slug or a bus id.
    if args.iter().skip(1).any(|a| a == "--help" || a == "-h") {
        return Command::Help;
    }
    match first.as_str() {
        "daemon" => Command::Daemon,
        "hook" => Command::Hook {
            harness: flag_value(&args, "--harness")
                .or(env_harness)
                .unwrap_or_else(|| DEFAULT_HARNESS.to_string()),
            statusline: args.iter().any(|a| a == "--statusline"),
            inference_listen: args.iter().any(|a| a == "--inference-listen").then(|| {
                flag_value(&args, "--inference-listen")
                    .filter(|value| !value.starts_with('-'))
                    .unwrap_or_else(|| "127.0.0.1:8787".into())
            }),
        },
        "install" => Command::Install {
            apply: args.iter().any(|a| a == "--apply"),
            managed: args.iter().any(|a| a == "--managed"),
        },
        "uninstall" => Command::Uninstall {
            apply: args.iter().any(|a| a == "--apply"),
        },
        "backfill" => Command::Backfill {
            apply: args.iter().any(|a| a == "--apply"),
            gaps: args.iter().any(|a| a == "--gaps"),
            chat_export: flag_value(&args, "--chat-export"),
        },
        "log" => Command::Log {
            json: args.iter().any(|a| a == "--json"),
            filter: LogFilter {
                bus_id: flag_value(&args, "--bus-id"),
                since: flag_value(&args, "--since"),
                tail: flag_value(&args, "--tail").and_then(|n| n.parse().ok()),
                grep: flag_value(&args, "--grep"),
                reveal: args.iter().any(|a| a == "--reveal"),
            },
        },
        "doctor" => Command::Doctor,
        "status" => Command::Status,
        "quota" => Command::Quota,
        "hop" => Command::Hop {
            from: flag_value(&args, "--from"),
            to: flag_value(&args, "--to").or_else(|| positional_dest(&args)),
            apply: args.iter().any(|a| a == "--apply"),
            demo: args.iter().any(|a| a == "--demo"),
        },
        "go" => Command::Hop {
            from: flag_value(&args, "--from"),
            to: flag_value(&args, "--to").or_else(|| positional_dest(&args)),
            apply: true,
            demo: false,
        },
        "summary" => Command::Summary {
            bus_id: flag_value(&args, "--bus-id"),
        },
        "verdict" => Command::Verdict,
        "usage" => Command::Usage {
            days: flag_value(&args, "--days")
                .and_then(|n| n.parse().ok())
                .unwrap_or(DEFAULT_USAGE_DAYS),
            since: flag_value(&args, "--since"),
            day: flag_value(&args, "--day"),
            json: args.iter().any(|a| a == "--json"),
            shareable: args.iter().any(|a| a == "--shareable"),
        },
        "run" => Command::Run {
            harness: args.get(1).cloned().unwrap_or_else(|| "claude_code".into()),
        },
        "arena" => Command::Arena {
            action: args.get(1).cloned().unwrap_or_else(|| "fact".into()),
            workspace: flag_value(&args, "--workspace"),
            kind: flag_value(&args, "--type"),
            claim: flag_value(&args, "--claim"),
            anchor: flag_value(&args, "--anchor"),
            from: flag_value(&args, "--from"),
            to: flag_value(&args, "--to"),
            bus_id: flag_value(&args, "--bus-id"),
            seq: flag_value(&args, "--seq").and_then(|n| n.parse().ok()),
        },
        "version" | "--version" | "-V" => Command::Version,
        "help" | "--help" | "-h" => Command::Help,
        other => Command::Unknown(other.to_string()),
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let position = args.iter().position(|a| a == flag)?;
    args.get(position + 1).cloned()
}

/// `conbus hop cursor` and `conbus go opencode` — a catalog slug, not a flag.
fn positional_dest(args: &[String]) -> Option<String> {
    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "--from" | "--to" => i += 2,
            "--apply" | "--demo" => i += 1,
            flag if flag.starts_with('-') => i += 1,
            name => return Some(name.to_string()),
        }
    }
    None
}

pub const USAGE: &str = "\
conbus — a conversation bus for coding agents

USAGE:
    conbus daemon                 run the recorder
    conbus hook [--harness NAME] [--statusline] [--inference-listen [ADDR]]
                                  mirror one vendor hook payload from stdin;
                                  --statusline reads Claude StatusLine JSON
                                  and must not print hook control output;
                                  an Inference hooks prompt frame
                                  (source.application=claude-ai) attaches as
                                  claude_chat and prints an allow verdict;
                                  --inference-listen is the official HTTPS
                                  terminator target (default 127.0.0.1:8787)
    conbus log [--json] [--bus-id ID] [--since TS] [--tail N] [--grep TEXT] [--reveal]
                                  read the conversation log back; --reveal
                                  decrypts sealed tool bodies (explicit only)
    conbus install [--apply]      register the hooks with every vendor found;
                                  reports unless told to write
    conbus install --managed      print, and never write, the hooks block for
                                  Claude Code's managed settings (fleets with
                                  allowManagedHooksOnly): JSON on stdout, where
                                  it goes and the Codex and Cursor managed
                                  hooks on stderr; docs/fleet.md
    conbus uninstall [--apply]    take them all back out; the log is kept
    conbus backfill [--apply] [--gaps] [--chat-export PATH]
                                  recover history; --gaps fills missed turns of live sessions;
                                  --chat-export reads official Claude.ai conversations.json (or zip)
    conbus doctor                 report what is and is not wired up
    conbus status                 routing rules, quota, hop history, cost
    conbus quota                  Codex probe + Claude StatusLine cache + catalog
    conbus hop [HARNESS] [--from ID] [--to HARNESS] [--apply] [--demo]
                                  decide the next harness; a positional name
                                  is --to; --apply records it and opens the
                                  destination (dual-warm on Claude/Codex;
                                  Cursor opens the checkout). A chat line
                                  that is only an order does the same.
    conbus go [HARNESS]           hop --apply for the current conversation
    conbus summary --bus-id ID    opposite-leg digest; never the protected leg
    conbus verdict                4-week dogfood score; open window is not a kill
    conbus usage [--days N] [--since DAY] [--day DAY] [--json] [--shareable]
                                  what the agents did, per harness: turns, stops,
                                  tool failures, permission asks (default 7 days;
                                  --day is one closed UTC day);
                                  --json prints one row per day, harness,
                                  workspace and attendance — counts, never bodies;
                                  --shareable prints what a sync would send:
                                  the same rows with no folder (docs/rfd/0002)
    conbus run [claude_code|codex|cursor|agy|opencode|grok]
                                  same TTY: start a vendor; hop execs the next
    conbus arena fact --workspace DIR --claim TEXT --anchor PATH
    conbus arena link --from DIR --to DIR
                                  accept a fact or link workspaces for the pack
    conbus --version              say which build this is
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_arguments_asks_for_help_rather_than_doing_something() {
        assert_eq!(parse(Vec::<String>::new(), None), Command::Help);
    }

    #[test]
    fn the_hook_defaults_to_claude_code() {
        assert_eq!(
            parse(["hook"], None),
            Command::Hook {
                harness: "claude_code".into(),
                statusline: false,
                inference_listen: None
            }
        );
    }

    #[test]
    fn an_explicit_harness_flag_beats_the_environment() {
        assert_eq!(
            parse(["hook", "--harness", "codex"], Some("cursor".into())),
            Command::Hook {
                harness: "codex".into(),
                statusline: false,
                inference_listen: None
            }
        );
    }

    #[test]
    fn the_environment_names_the_harness_when_the_flag_does_not() {
        assert_eq!(
            parse(["hook"], Some("codex".into())),
            Command::Hook {
                harness: "codex".into(),
                statusline: false,
                inference_listen: None
            }
        );
    }

    #[test]
    fn inference_listen_defaults_the_official_loopback() {
        assert_eq!(
            parse(["hook", "--inference-listen"], None),
            Command::Hook {
                harness: "claude_code".into(),
                statusline: false,
                inference_listen: Some("127.0.0.1:8787".into())
            }
        );
        assert_eq!(
            parse(["hook", "--inference-listen", "127.0.0.1:0"], None),
            Command::Hook {
                harness: "claude_code".into(),
                statusline: false,
                inference_listen: Some("127.0.0.1:0".into())
            }
        );
    }

    #[test]
    fn statusline_is_a_hook_flag() {
        assert_eq!(
            parse(["hook", "--statusline"], None),
            Command::Hook {
                harness: "claude_code".into(),
                statusline: true,
                inference_listen: None
            }
        );
    }

    #[test]
    fn hop_demo_and_summary_and_verdict_are_commands() {
        assert_eq!(
            parse(["hop", "--demo"], None),
            Command::Hop {
                from: None,
                to: None,
                apply: false,
                demo: true
            }
        );
        assert_eq!(
            parse(["summary", "--bus-id", "01a0"], None),
            Command::Summary {
                bus_id: Some("01a0".into())
            }
        );
        assert_eq!(parse(["verdict"], None), Command::Verdict);
    }

    #[test]
    fn hop_is_a_command() {
        assert_eq!(
            parse(["hop"], None),
            Command::Hop {
                from: None,
                to: None,
                apply: false,
                demo: false
            }
        );
        assert_eq!(
            parse(
                ["hop", "--from", "01996b1e", "--to", "codex", "--apply"],
                None
            ),
            Command::Hop {
                from: Some("01996b1e".into()),
                to: Some("codex".into()),
                apply: true,
                demo: false
            }
        );
        assert_eq!(
            parse(["hop", "cursor"], None),
            Command::Hop {
                from: None,
                to: Some("cursor".into()),
                apply: false,
                demo: false
            }
        );
        assert_eq!(
            parse(["go", "opencode"], None),
            Command::Hop {
                from: None,
                to: Some("opencode".into()),
                apply: true,
                demo: false
            }
        );
        assert_eq!(parse(["go", "--help"], None), Command::Help);
        assert_eq!(parse(["hop", "-h"], None), Command::Help);
    }

    #[test]
    fn usage_is_a_week_unless_told_otherwise() {
        assert_eq!(
            parse(["usage"], None),
            Command::Usage {
                days: 7,
                since: None,
                day: None,
                json: false,
                shareable: false
            }
        );
        assert_eq!(
            parse(["usage", "--days", "30", "--json"], None),
            Command::Usage {
                days: 30,
                since: None,
                day: None,
                json: true,
                shareable: false
            }
        );
        assert_eq!(
            parse(["usage", "--since", "2026-09-01"], None),
            Command::Usage {
                days: 7,
                since: Some("2026-09-01".into()),
                day: None,
                json: false,
                shareable: false
            }
        );
        assert_eq!(
            parse(["usage", "--days", "lots"], None),
            Command::Usage {
                days: 7,
                since: None,
                day: None,
                json: false,
                shareable: false
            },
            "a count that is not a number is ignored, the way --tail is"
        );
        assert_eq!(
            parse(["usage", "--shareable"], None),
            Command::Usage {
                days: 7,
                since: None,
                day: None,
                json: false,
                shareable: true
            }
        );
        assert_eq!(
            parse(["usage", "--day", "2026-09-22", "--shareable"], None),
            Command::Usage {
                days: 7,
                since: None,
                day: Some("2026-09-22".into()),
                json: false,
                shareable: true
            }
        );
    }

    #[test]
    fn status_is_a_command() {
        assert_eq!(parse(["status"], None), Command::Status);
    }

    #[test]
    fn run_and_arena_are_commands() {
        assert_eq!(
            parse(["run", "codex"], None),
            Command::Run {
                harness: "codex".into()
            }
        );
        assert_eq!(
            parse(["arena", "link", "--from", "/a", "--to", "/b"], None),
            Command::Arena {
                action: "link".into(),
                workspace: None,
                kind: None,
                claim: None,
                anchor: None,
                from: Some("/a".into()),
                to: Some("/b".into()),
                bus_id: None,
                seq: None
            }
        );
    }

    #[test]
    fn backfill_gaps_is_a_flag() {
        assert_eq!(
            parse(["backfill"], None),
            Command::Backfill {
                apply: false,
                gaps: false,
                chat_export: None
            }
        );
        assert_eq!(
            parse(["backfill", "--gaps", "--apply"], None),
            Command::Backfill {
                apply: true,
                gaps: true,
                chat_export: None
            }
        );
        assert_eq!(
            parse(
                ["backfill", "--chat-export", "/tmp/conversations.json"],
                None
            ),
            Command::Backfill {
                apply: false,
                gaps: false,
                chat_export: Some("/tmp/conversations.json".into())
            }
        );
    }

    #[test]
    fn the_quota_probe_is_a_command() {
        // Probe #1 of the plan, answered on 2026-09-03: Codex's app-server
        // hands out the account's rate limits to any client. The router's
        // whole premise is that this signal exists; a command makes it a
        // thing a person can look at.
        assert_eq!(parse(["quota"], None), Command::Quota);
    }

    #[test]
    fn the_binary_can_say_which_build_it_is() {
        // The first audit had to run `strings` on the installed binary to
        // guess which commit it was, and the deploy after the second one
        // opened a window where doctor and the daemon ran different code
        // with nothing to say so. A build that cannot name itself cannot be
        // compared to anything.
        assert_eq!(parse(["--version"], None), Command::Version);
        assert_eq!(parse(["-V"], None), Command::Version);
        assert_eq!(parse(["version"], None), Command::Version);
    }

    #[test]
    fn an_unknown_subcommand_is_named_back() {
        assert_eq!(
            parse(["frobnicate"], None),
            Command::Unknown("frobnicate".into())
        );
    }

    #[test]
    fn install_reports_unless_told_to_apply() {
        // The default has to be the safe one: these are the user's own settings
        // files and they usually already hold their own hooks.
        assert_eq!(
            parse(["install"], None),
            Command::Install {
                apply: false,
                managed: false
            }
        );
        assert_eq!(
            parse(["install", "--apply"], None),
            Command::Install {
                apply: true,
                managed: false
            }
        );
    }

    #[test]
    fn install_managed_is_a_flag_of_its_own() {
        // The block an administrator puts in managed settings. It only ever
        // prints; `main` lets --managed win over --apply, so the parse keeps
        // both and the report says --apply did nothing.
        assert_eq!(
            parse(["install", "--managed"], None),
            Command::Install {
                apply: false,
                managed: true
            }
        );
        assert_eq!(
            parse(["install", "--managed", "--apply"], None),
            Command::Install {
                apply: true,
                managed: true
            }
        );
        assert_eq!(
            parse(["install", "--managed", "--help"], None),
            Command::Help
        );
    }

    #[test]
    fn uninstall_reports_unless_told_to_apply() {
        assert_eq!(
            parse(["uninstall"], None),
            Command::Uninstall { apply: false }
        );
        assert_eq!(
            parse(["uninstall", "--apply"], None),
            Command::Uninstall { apply: true }
        );
    }

    #[test]
    fn log_takes_a_json_flag() {
        assert_eq!(
            parse(["log"], None),
            Command::Log {
                json: false,
                filter: LogFilter::default()
            }
        );
        assert_eq!(
            parse(["log", "--json"], None),
            Command::Log {
                json: true,
                filter: LogFilter::default()
            }
        );
    }

    #[test]
    fn log_takes_the_selectors_that_make_a_long_log_usable() {
        let Command::Log { filter, .. } = parse(
            [
                "log",
                "--bus-id",
                "01996b1e",
                "--since",
                "2026-08-31",
                "--tail",
                "20",
                "--grep",
                "rate_limit",
            ],
            None,
        ) else {
            panic!("expected a log command");
        };

        assert_eq!(filter.bus_id.as_deref(), Some("01996b1e"));
        assert_eq!(filter.since.as_deref(), Some("2026-08-31"));
        assert_eq!(filter.tail, Some(20));
        assert_eq!(filter.grep.as_deref(), Some("rate_limit"));
    }

    #[test]
    fn a_tail_that_is_not_a_number_is_ignored_rather_than_failing_the_command() {
        let Command::Log { filter, .. } = parse(["log", "--tail", "lots"], None) else {
            panic!("expected a log command");
        };

        assert_eq!(filter.tail, None);
    }
}
