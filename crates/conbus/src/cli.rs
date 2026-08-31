//! One binary, argv dispatch. Deployment is a single file, and the hook has to
//! be registered by absolute path anyway because a vendor's `PATH` is not ours.

/// How the binary was invoked.
#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Command {
    Daemon,
    /// Read a vendor hook payload on stdin and mirror it.
    Hook {
        harness: String,
    },
    Log {
        json: bool,
        filter: LogFilter,
    },
    /// Register the hook with the harnesses. Reports by default; `--apply`
    /// writes, after backing up whatever was there.
    Install {
        apply: bool,
    },
    Doctor,
    Help,
    Unknown(String),
}

/// The harness assumed when nothing says otherwise. Stage 0 registers against
/// Claude Code first; the other harnesses that read the same settings format
/// arrive with `--harness`.
pub const DEFAULT_HARNESS: &str = conbus_core::event::Harness::CLAUDE_CODE;

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
    match first.as_str() {
        "daemon" => Command::Daemon,
        "hook" => Command::Hook {
            harness: flag_value(&args, "--harness")
                .or(env_harness)
                .unwrap_or_else(|| DEFAULT_HARNESS.to_string()),
        },
        "install" => Command::Install {
            apply: args.iter().any(|a| a == "--apply"),
        },
        "log" => Command::Log {
            json: args.iter().any(|a| a == "--json"),
            filter: LogFilter {
                bus_id: flag_value(&args, "--bus-id"),
                since: flag_value(&args, "--since"),
                tail: flag_value(&args, "--tail").and_then(|n| n.parse().ok()),
                grep: flag_value(&args, "--grep"),
            },
        },
        "doctor" => Command::Doctor,
        "help" | "--help" | "-h" => Command::Help,
        other => Command::Unknown(other.to_string()),
    }
}

fn flag_value(args: &[String], flag: &str) -> Option<String> {
    let position = args.iter().position(|a| a == flag)?;
    args.get(position + 1).cloned()
}

pub const USAGE: &str = "\
conbus — a conversation bus for coding agents

USAGE:
    conbus daemon                 run the recorder
    conbus hook [--harness NAME]  mirror one vendor hook payload from stdin
    conbus log [--json] [--bus-id ID] [--since TS] [--tail N] [--grep TEXT]
                                  read the conversation log back
    conbus install [--apply]      register the hook with Claude Code and Codex
    conbus doctor                 report what is and is not wired up
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
                harness: "claude_code".into()
            }
        );
    }

    #[test]
    fn an_explicit_harness_flag_beats_the_environment() {
        assert_eq!(
            parse(["hook", "--harness", "codex"], Some("cursor".into())),
            Command::Hook {
                harness: "codex".into()
            }
        );
    }

    #[test]
    fn the_environment_names_the_harness_when_the_flag_does_not() {
        assert_eq!(
            parse(["hook"], Some("codex".into())),
            Command::Hook {
                harness: "codex".into()
            }
        );
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
        assert_eq!(parse(["install"], None), Command::Install { apply: false });
        assert_eq!(
            parse(["install", "--apply"], None),
            Command::Install { apply: true }
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
