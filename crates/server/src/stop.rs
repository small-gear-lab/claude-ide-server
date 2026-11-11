//! What stops the server from outside: the signals of the system.
//!
//! On Unix that is `SIGINT` and `SIGTERM` (a supervisor ends a service with `SIGTERM`). Windows has no signals; it has
//! console control events instead: Ctrl-C and Ctrl-Break, and the closing of the console and the logging off or shutting
//! down of the system, which come with a few seconds to finish in. Both ways say what happened with the same two words, for
//! the log.

use std::io;

/// What waits for the server to be told to stop.
pub struct Stop {
    #[cfg(unix)]
    sigint: tokio::signal::unix::Signal,
    #[cfg(unix)]
    sigterm: tokio::signal::unix::Signal,
    #[cfg(windows)]
    ctrl_c: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    ctrl_break: tokio::signal::windows::CtrlBreak,
    #[cfg(windows)]
    ctrl_close: tokio::signal::windows::CtrlClose,
    #[cfg(windows)]
    ctrl_shutdown: tokio::signal::windows::CtrlShutdown,
}

impl Stop {
    /// Starts listening. Must be called inside a tokio runtime, and before the server does anything that a stop would
    /// have to undo.
    #[cfg(unix)]
    pub fn new() -> io::Result<Self> {
        use tokio::signal::unix::{SignalKind, signal};
        Ok(Self {
            sigint: signal(SignalKind::interrupt())?,
            sigterm: signal(SignalKind::terminate())?,
        })
    }

    #[cfg(windows)]
    pub fn new() -> io::Result<Self> {
        use tokio::signal::windows;
        Ok(Self {
            ctrl_c: windows::ctrl_c()?,
            ctrl_break: windows::ctrl_break()?,
            ctrl_close: windows::ctrl_close()?,
            ctrl_shutdown: windows::ctrl_shutdown()?,
        })
    }

    /// Waits until the server is told to stop. `"interrupted"` is a user at the keyboard (Ctrl-C), `"terminated"` is
    /// whoever runs the service (the console was closed, the system is going down; on Unix, `SIGTERM`).
    #[cfg(unix)]
    pub async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.sigint.recv() => "interrupted",
            _ = self.sigterm.recv() => "terminated",
        }
    }

    #[cfg(windows)]
    pub async fn recv(&mut self) -> &'static str {
        tokio::select! {
            _ = self.ctrl_c.recv() => "interrupted",
            _ = self.ctrl_break.recv() => "interrupted",
            _ = self.ctrl_close.recv() => "terminated",
            _ = self.ctrl_shutdown.recv() => "terminated",
        }
    }
}
