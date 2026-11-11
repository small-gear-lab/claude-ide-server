//! The editor-agnostic core of claude-ide-server: the Claude Code `/ide` protocol side.
//!
//! Three sides, which meet only in [`workspace`]:
//!
//! - the Claude side: [`ws`] (the WebSocket server that Claude Code connects to), [`lock`] (the lock files that it
//!   finds) and [`mcp`] (the MCP protocol, on `rmcp`, behind this crate's own types);
//! - the adapter side: [`adapter`] (editors register here and answer requests) and [`jsonrpc`] (its messages and
//!   framing);
//! - the middle: [`workspace`], which gives each registered adapter a port and a lock file, and runs the tools.
//!
//! The design is in `docs/DESIGN.md`.

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
