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
    if options.print_socket {
        // Where the adapters connect, for an adapter that cannot work it out as the server does (on Windows the name of the
        // pipe has the security identifier of the user in it, which a Lua program does not have).
        let path = options.socket.clone().unwrap_or_else(|| {
            claude_ide_core::workspace::socket_path(&claude_ide_core::lock::EnvVars::from_process())
        });
        println!("{}", path.display());
        return ExitCode::SUCCESS;
    }
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
