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
    },
    Doctor,
    Help,
    Unknown(String),
}

/// The harness assumed when nothing says otherwise. Stage 0 registers against
/// Claude Code first; the other harnesses that read the same settings format
/// arrive with `--harness`.
pub const DEFAULT_HARNESS: &str = conbus_core::event::Harness::CLAUDE_CODE;

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
        "log" => Command::Log {
            json: args.iter().any(|a| a == "--json"),
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
    conbus log [--json]           read the conversation log back
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
    fn log_takes_a_json_flag() {
        assert_eq!(parse(["log"], None), Command::Log { json: false });
        assert_eq!(parse(["log", "--json"], None), Command::Log { json: true });
    }
}
