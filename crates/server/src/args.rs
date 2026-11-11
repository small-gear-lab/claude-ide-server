//! The command line: a few switches, read by hand (an argument-parser crate would be more than they need).

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

pub const USAGE: &str = "\
Usage: claude-ide-server [options]

Runs the server that connects editors to Claude Code (/ide).

Options:
  --socket <path>        Where the adapters connect. Default: adapter.sock in the runtime directory
                         (on Windows, the named pipe \\\\.\\pipe\\claude-ide-server-<sid>, and a path is a pipe name).
                         CLAUDE_IDE_SERVER_SOCKET sets it for the server and the adapters that read it, and --socket
                         wins over it. Ignored when the socket is handed over by whatever started the server.
  --print-socket         Print where the adapters connect (the --socket, or the default) and exit: for an
                         adapter that has to know it, and cannot work it out as the server does.
  --idle-timeout <secs>  Exit when the last adapter has been gone for this long. Default: never.
  -v, --verbose          More detail in the log; twice for the most.
  -q, --quiet            Log only warnings and errors.
  -h, --help             Show this text.
  -V, --version          Show the version.

The log goes to standard error. RUST_LOG (or CLAUDE_IDE_SERVER_LOG) overrides -v and -q.
";

/// What the command line asks for.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Options),
    Help,
    Version,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Options {
    pub socket: Option<PathBuf>,
    /// Only say where the adapters connect, and end.
    pub print_socket: bool,
    pub idle_timeout: Option<Duration>,
    /// 0 is the usual level; each `-v` is one more, each `-q` one less.
    pub verbosity: i8,
}

/// Reads the arguments (without the program name). The error is a message for the user.
pub fn parse(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut options = Options::default();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        let arg = arg
            .into_string()
            .map_err(|arg| format!("the argument {arg:?} is not valid text"))?;
        let (name, inline) = match arg.split_once('=') {
            Some((name, value)) if name.starts_with("--") => {
                (name.to_owned(), Some(value.to_owned()))
            }
            _ => (arg.clone(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            match inline.clone() {
                Some(value) => Ok(value),
                None => args
                    .next()
                    .ok_or_else(|| format!("{name} needs a value"))?
                    .into_string()
                    .map_err(|_| format!("the value of {name} is not valid text")),
            }
        };
        // A switch that takes no value is not given one with `=`.
        if inline.is_some() && !matches!(name.as_str(), "--socket" | "--idle-timeout") {
            return Err(format!("unknown argument {arg:?}"));
        }
        match name.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "-V" | "--version" => return Ok(Command::Version),
            "-v" | "--verbose" => options.verbosity = options.verbosity.saturating_add(1),
            "-q" | "--quiet" => options.verbosity = options.verbosity.saturating_sub(1),
            "--socket" => options.socket = Some(PathBuf::from(value("--socket")?)),
            "--print-socket" => options.print_socket = true,
            "--idle-timeout" => {
                let text = value("--idle-timeout")?;
                let seconds: u64 = text.parse().map_err(|_| {
                    format!("--idle-timeout needs a number of seconds, not {text:?}")
                })?;
                // Zero is "never", as in the default: an idle timeout of nothing would stop the server at once.
                options.idle_timeout = (seconds > 0).then(|| Duration::from_secs(seconds));
            }
            "-vv" => options.verbosity = options.verbosity.saturating_add(2),
            _ => return Err(format!("unknown argument {arg:?}")),
        }
    }
    Ok(Command::Run(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_strs(args: &[&str]) -> Result<Command, String> {
        parse(args.iter().map(OsString::from))
    }

    #[test]
    fn no_arguments_run_with_the_defaults() {
        assert_eq!(parse_strs(&[]), Ok(Command::Run(Options::default())));
    }

    #[test]
    fn the_switches_are_read() {
        let command = parse_strs(&["--socket", "/run/a.sock", "--idle-timeout=30", "-v", "-v"]);
        assert_eq!(
            command,
            Ok(Command::Run(Options {
                socket: Some("/run/a.sock".into()),
                idle_timeout: Some(Duration::from_secs(30)),
                verbosity: 2,
                ..Options::default()
            }))
        );
        assert_eq!(
            parse_strs(&["--print-socket"]),
            Ok(Command::Run(Options {
                print_socket: true,
                ..Options::default()
            }))
        );
        assert_eq!(
            parse_strs(&["-q"]),
            Ok(Command::Run(Options {
                verbosity: -1,
                ..Options::default()
            }))
        );
    }

    #[test]
    fn zero_seconds_of_idle_time_means_never() {
        assert_eq!(
            parse_strs(&["--idle-timeout", "0"]),
            Ok(Command::Run(Options::default()))
        );
    }

    #[test]
    fn help_and_version_win() {
        assert_eq!(parse_strs(&["-h"]), Ok(Command::Help));
        assert_eq!(parse_strs(&["--version"]), Ok(Command::Version));
    }

    #[test]
    fn mistakes_are_told_to_the_user() {
        assert!(
            parse_strs(&["--socket"])
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(
            parse_strs(&["--idle-timeout", "soon"])
                .unwrap_err()
                .contains("number of seconds")
        );
        assert!(
            parse_strs(&["--nope"])
                .unwrap_err()
                .contains("unknown argument")
        );
    }
}
