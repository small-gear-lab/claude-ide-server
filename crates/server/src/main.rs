//! `claude-ide-server`: the server that connects editors to Claude Code (/ide).

#[cfg(unix)]
mod activation;
mod args;
#[cfg(unix)]
mod logging;
#[cfg(unix)]
mod serve;

use std::process::ExitCode;

use args::{Command, USAGE};

#[cfg(unix)]
fn main() -> ExitCode {
    let options = match args::parse(std::env::args_os().skip(1)) {
        Ok(Command::Run(options)) => options,
        Ok(Command::Help) => {
            print!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(Command::Version) => {
            println!("claude-ide-server {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(message) => {
            eprintln!("claude-ide-server: {message}\n\n{USAGE}");
            return ExitCode::from(serve::exit::USAGE);
        }
    };
    logging::init(options.verbosity);
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("claude-ide-server: could not start the runtime: {error}");
            return ExitCode::from(serve::exit::START_FAILED);
        }
    };
    ExitCode::from(runtime.block_on(serve::run(options)))
}

/// The adapter socket is a Unix domain socket for now; a Windows endpoint is a separate piece of work.
#[cfg(not(unix))]
fn main() -> ExitCode {
    let _ = (Command::Help, USAGE);
    eprintln!("claude-ide-server: this system is not supported yet");
    ExitCode::from(1)
}
