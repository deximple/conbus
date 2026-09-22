// Declared first: `note!` is a macro_rules, so it is in scope only for the
// modules that come after it, and the daemon is one of them.
#[macro_use]
mod note;

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
mod intent;
mod landing;
mod quota;
mod report;
mod runner;
mod summarise;
mod takeover;
mod uninstall;
mod verdict;
mod warm;
mod window;
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
            inference_listen,
        } => {
            if let Some(addr) = inference_listen {
                return run(|_| hook::inference::serve(&addr, &harness));
            }
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
                    quota::observe_statusline(paths.as_ref(), forward.as_deref(), &stdin)
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
            let mut payload = String::new();
            let _ = std::io::stdin().read_to_string(&mut payload);
            let reply = match hook::client::run(&harness, &mut payload.as_bytes()) {
                Ok(reply) => reply,
                Err(err) => {
                    // Durable, because the vendor swallows stderr; throttled,
                    // because a stopped daemon would otherwise write one of
                    // these per hook per turn straight into the transcript.
                    if drops::record(&err) {
                        eprintln!("conbus hook: record dropped: {err}");
                    }
                    hook::client::failure_reply(&harness, &payload).to_string()
                }
            };
            // One JSON object and nothing else. The vendor's parser runs
            // `JSON.parse` on the whole of stdout, so a stray line anywhere
            // turns a reply into plain text — which on `UserPromptSubmit` is
            // injected verbatim rather than ignored.
            println!("{reply}");
            ExitCode::SUCCESS
        }
        Command::Daemon => run(|paths| daemon::serve(paths, Retention::from_env())),
        Command::Log { json, filter } => run(|paths| report::log(paths, json, &filter)),
        Command::Install { apply } => run(|_| report::install(apply)),
        Command::Uninstall { apply } => run(|_| uninstall::run(apply)),
        Command::Backfill {
            apply,
            gaps,
            chat_export,
        } => run(|paths| {
            if let Some(export) = chat_export {
                backfill::run_chat_export(paths, &export, apply)
            } else if gaps {
                backfill::run_gaps(paths, apply)
            } else {
                backfill::run(paths, apply)
            }
        }),
        Command::Doctor => run(report::doctor),
        Command::Status => run(report::status),
        Command::Quota => run(quota::run),
        Command::Hop {
            from,
            to,
            apply,
            demo,
        } => {
            if demo {
                match hop::demo() {
                    Ok(()) => ExitCode::SUCCESS,
                    Err(err) => {
                        eprintln!("conbus: {err}");
                        ExitCode::FAILURE
                    }
                }
            } else {
                run(|paths| hop::run(paths, from, to, apply))
            }
        }
        Command::Summary { bus_id } => run(|paths| summarise::run(paths, bus_id)),
        Command::Verdict => run(verdict::run),
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
            println!("conbus {}", build_info::describe());
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
