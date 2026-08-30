mod cli;
mod daemon;
mod harden;
mod hook;
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
            if let Err(err) = hook::client::run(&harness, &mut std::io::stdin()) {
                eprintln!("conbus hook: record dropped: {err}");
            }
            ExitCode::SUCCESS
        }
        Command::Daemon => run(|paths| daemon::serve(paths, Retention::default())),
        Command::Log { json } => run(|paths| report::log(paths, json)),
        Command::Doctor => run(report::doctor),
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
