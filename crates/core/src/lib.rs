//! The editor-agnostic core of claude-ide-server: the Claude Code `/ide` protocol side.

#[cfg(unix)]
pub mod lock;
pub mod ws;
