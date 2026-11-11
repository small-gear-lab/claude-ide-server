//! `claude-ide-server`: the server that connects editors to Claude Code (/ide).

mod activation;
mod args;
mod logging;
mod serve;
mod stop;

use std::process::ExitCode;

use args::{Command, USAGE};

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
