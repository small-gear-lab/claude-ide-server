//! The editor-agnostic core of claude-ide-server: the Claude Code `/ide` protocol side.

pub mod jsonrpc;
#[cfg(unix)]
pub mod lock;
pub mod ws;
