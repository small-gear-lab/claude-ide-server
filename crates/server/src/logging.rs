//! The log: `tracing`, to standard error. Colours only on a terminal, and not if `NO_COLOR` is set.

use std::io::IsTerminal;

use tracing_subscriber::EnvFilter;

/// The filter for the verbosity of the command line (`-q` is -1, `-v` is 1): the server's own crates at that level, and
/// everything else (the libraries) only at warnings.
pub fn default_filter(verbosity: i8) -> String {
    let level = match verbosity {
        i8::MIN..=-1 => "warn",
        0 => "info",
        1 => "debug",
        2..=i8::MAX => "trace",
    };
    format!("warn,claude_ide_server={level},claude_ide_core={level}")
}

/// Starts logging. `CLAUDE_IDE_SERVER_LOG`, or else `RUST_LOG`, replaces what the command line asked for.
pub fn init(verbosity: i8) {
    let filter = ["CLAUDE_IDE_SERVER_LOG", "RUST_LOG"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .and_then(|spec| EnvFilter::try_new(spec).ok())
        .unwrap_or_else(|| EnvFilter::new(default_filter(verbosity)));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none())
        .with_target(false)
        .init();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_sets_the_level_of_our_own_crates() {
        assert!(default_filter(-1).contains("claude_ide_server=warn"));
        assert!(default_filter(0).contains("claude_ide_server=info"));
        assert!(default_filter(1).contains("claude_ide_core=debug"));
        assert!(default_filter(5).contains("claude_ide_server=trace"));
        assert!(default_filter(0).starts_with("warn,"));
    }
}
