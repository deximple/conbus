mod appserver;
mod arena;
mod backfill;
mod build_info;
mod cli;
mod code;
mod daemon;
mod drops;
mod harden;
mod hook;
mod hop;
mod install;
mod quota;
mod report;
mod runner;
mod takeover;
mod warm;
mod wire;

use std::io::Read;
use std::process::ExitCode;

use conbus_core::paths::Paths;
use conbus_core::retention::Retention;

use cli::Command;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args, std::env::var("CONBUS_HARNESS").ok()) {
        // A hook must never fail the turn it is attached to, so every failure
        // below leaves on stderr and exits zero.
        Command::Hook {
            harness,
            statusline,
        } => {
            if statusline {
                // StatusLine stdout is the status bar. Hook JSON here would
                // paint `{"continue":true}` into the TUI.
                let mut stdin = String::new();
                let _ = std::io::stdin().read_to_string(&mut stdin);
                let paths = Paths::resolve().ok();
                let forward = std::env::var_os("HOME").map(|home| {
                    std::path::PathBuf::from(home)
                        .join(".claude")
                        .join("conbus-statusline-forward")
                });
                print!(
                    "{}",
                    quota::observe_statusline(
                        paths.as_ref(),
                        forward.as_deref(),
                        &stdin
                    )
                );
                return ExitCode::SUCCESS;
            }
            // Say something well-formed, always — and say it whatever went
            // wrong, which is why the failure path falls back to the silent
            // reply rather than to nothing.
            //
            // Writing nothing looked safe and was not. Installed on a real
            // machine, a hook that exited zero with no stdout coincided with
            // `400 cache_control cannot be set for empty text blocks` breaking
            // the session it was attached to, and reverting the hook stopped
            // it. The vendor's documented contract takes JSON on stdout, so an
            // explicit "carry on" costs nothing and cannot be mistaken for an
            // empty context block.
            let reply = match hook::client::run(&harness, &mut std::io::stdin()) {
                Ok(reply) => reply,
                Err(err) => {
                    // Durable, because the vendor swallows stderr; throttled,
                    // because a stopped daemon would otherwise write one of
                    // these per hook per turn straight into the transcript.
                    if drops::record(&err) {
                        eprintln!("conbus hook: record dropped: {err}");
                    }
                    hook::client::silent_reply(&harness).to_string()
                }
            };
            // One JSON object and nothing else. The vendor's parser runs
            // `JSON.parse` on the whole of stdout, so a stray line anywhere
            // turns a reply into plain text — which on `UserPromptSubmit` is
            // injected verbatim rather than ignored.
            println!("{reply}");
            ExitCode::SUCCESS
        }
        Command::Daemon => run(|paths| daemon::serve(paths, Retention::default())),
        Command::Log { json, filter } => run(|paths| report::log(paths, json, &filter)),
        Command::Install { apply } => run(|_| report::install(apply)),
        Command::Backfill { apply, gaps } => run(|paths| {
            if gaps {
                backfill::run_gaps(paths, apply)
            } else {
                backfill::run(paths, apply)
            }
        }),
        Command::Doctor => run(report::doctor),
        Command::Status => run(report::status),
        Command::Quota => run(quota::run),
        Command::Hop { from, to, apply } => run(|paths| hop::run(paths, from, to, apply)),
        Command::Run { harness } => run(|paths| runner::run(paths, &harness)),
        Command::Arena {
            action,
            workspace,
            kind,
            claim,
            anchor,
            from,
            to,
            bus_id,
            seq,
        } => run(|paths| {
            arena::run(
                paths, &action, workspace, kind, claim, anchor, from, to, bus_id, seq,
            )
        }),
        Command::Version => {
            println!("conbus {}", build_info::BUILD);
            ExitCode::SUCCESS
        }
        Command::Help => {
            print!("{}", cli::USAGE);
            ExitCode::SUCCESS
        }
        Command::Unknown(name) => {
            eprintln!("conbus: unknown command {name:?}\n\n{}", cli::USAGE);
            ExitCode::FAILURE
        }
    }
}

fn run(action: impl FnOnce(&Paths) -> std::io::Result<()>) -> ExitCode {
    let Ok(paths) = Paths::resolve() else {
        eprintln!("conbus: no home directory; set CONBUS_HOME");
        return ExitCode::FAILURE;
    };
    match action(&paths) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("conbus: {err}");
            ExitCode::FAILURE
        }
    }
}
