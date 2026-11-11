//! The editor-agnostic core of claude-ide-server: the Claude Code `/ide` protocol side.

pub mod adapter;
#[cfg(unix)]
mod fsutil;
pub mod jsonrpc;
#[cfg(unix)]
pub mod lock;
pub mod mcp;
mod tools;
#[cfg(unix)]
pub mod workspace;
pub mod ws;
