mod backfill;
mod build_info;
mod cli;
mod daemon;
mod drops;
mod harden;
mod hook;
mod install;
mod report;
mod wire;

use std::process::ExitCode;

use conbus_core::paths::Paths;
use conbus_core::retention::Retention;

use cli::Command;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match cli::parse(&args, std::env::var("CONBUS_HARNESS").ok()) {
        // A hook must never fail the turn it is attached to, so every failure
        // below leaves on stderr and exits zero.
        Command::Hook { harness } => {
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
        Command::Backfill { apply } => run(|paths| backfill::run(paths, apply)),
        Command::Doctor => run(report::doctor),
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
