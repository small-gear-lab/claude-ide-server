//! Running the server: get the socket, serve until told to stop (or until idle), and say what happens.

use std::sync::Arc;
use std::time::Duration;

use claude_ide_core::adapter;
use claude_ide_core::lock::{EnvVars, LockDirs};
use claude_ide_core::mcp::ClientChange;
use claude_ide_core::workspace::{Config, Workspace, WorkspaceEvent, WorkspaceServer, socket_path};
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tracing::{debug, error, info, warn};

use crate::activation::{default_sources, open_first};
use crate::args::Options;

/// How the process ends.
pub mod exit {
    /// Stopped as asked: a signal, or no adapter for as long as `--idle-timeout`.
    pub const OK: u8 = 0;
    /// It could not start (no socket, no place for the lock files), or the socket closed under it.
    pub const START_FAILED: u8 = 1;
    /// The command line is wrong.
    pub const USAGE: u8 = 2;
    /// It was told to stop and did not finish in time; lock files may be left (the next server cleans them up).
    pub const STOP_FAILED: u8 = 3;
}

/// How long to wait for the workspaces to be closed when stopping.
const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs until stopped. Returns the exit code.
pub async fn run(options: Options) -> u8 {
    let env = EnvVars::from_process();
    let path = options.socket.clone().unwrap_or_else(|| socket_path(&env));
    let (listener, source) = match open_first(&default_sources(path)) {
        Ok(opened) => opened,
        Err(error) => {
            error!("could not get the socket for the adapters: {error}");
            return exit::START_FAILED;
        }
    };
    match listener.path() {
        Some(path) => info!("listening for adapters on {} ({source})", path.display()),
        None => info!("listening for adapters ({source})"),
    }
    let lock_dirs = match LockDirs::from_env(&env) {
        Ok(dirs) => dirs,
        Err(error) => {
            error!("could not find where the lock files go: {error}");
            return exit::START_FAILED;
        }
    };
    let (mut sigint, mut sigterm) = match (
        signal(SignalKind::interrupt()),
        signal(SignalKind::terminate()),
    ) {
        (Ok(sigint), Ok(sigterm)) => (sigint, sigterm),
        (Err(error), _) | (_, Err(error)) => {
            error!("could not listen for signals: {error}");
            return exit::START_FAILED;
        }
    };

    let mut config = Config::new(lock_dirs, std::process::id());
    config.claude_observer = Some(Arc::new(|registration, change| match change {
        ClientChange::Connected(client) => info!(
            "{} {} connected to the workspace of {} (pid {})",
            client.name, client.version, registration.ide_name, registration.pid
        ),
        ClientChange::Disconnected(client) => info!(
            "{} {} (pid {}) is gone from the workspace of {} (pid {})",
            client.name,
            client.version,
            client
                .pid
                .map_or("unknown".to_owned(), |pid| pid.to_string()),
            registration.ide_name,
            registration.pid
        ),
    }));
    let mut server = WorkspaceServer::start(listener, adapter::Config::default(), config);
    for removed in server.stale_removed() {
        info!(
            "removed the lock file of an earlier server: {}",
            removed.display()
        );
    }

    let (gone_tx, mut gone) = mpsc::unbounded_channel::<()>();
    let mut adapters = 0usize;
    // The idle time counts from the start, too: a server that is started on demand and never gets an adapter ends.
    let mut idle_since = options.idle_timeout.map(|timeout| Instant::now() + timeout);
    let code = loop {
        let idle = async {
            match idle_since {
                Some(deadline) => tokio::time::sleep_until(deadline).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            workspace = server.accept() => match workspace {
                Some(workspace) => {
                    adapters += 1;
                    idle_since = None;
                    tokio::spawn(watch(workspace, gone_tx.clone()));
                }
                None => {
                    error!("the socket for the adapters is closed");
                    break exit::START_FAILED;
                }
            },
            Some(()) = gone.recv() => {
                adapters -= 1;
                if adapters == 0 {
                    idle_since = options.idle_timeout.map(|timeout| Instant::now() + timeout);
                }
            }
            () = idle => {
                info!("no adapter for {} seconds: stopping", options.idle_timeout.map_or(0, |t| t.as_secs()));
                break exit::OK;
            }
            _ = sigint.recv() => { info!("interrupted: stopping"); break exit::OK; }
            _ = sigterm.recv() => { info!("terminated: stopping"); break exit::OK; }
        }
    };

    match tokio::time::timeout(STOP_TIMEOUT, server.shutdown()).await {
        Ok(()) => {
            info!("stopped");
            code
        }
        Err(_) => {
            error!(
                "the workspaces were not closed in {} seconds",
                STOP_TIMEOUT.as_secs()
            );
            exit::STOP_FAILED
        }
    }
}

/// Follows one workspace until its adapter is gone, and says what happens in it.
async fn watch(mut workspace: Workspace, gone: mpsc::UnboundedSender<()>) {
    let registration = workspace.registration();
    info!(
        "adapter registered: {} (pid {}), port {}, folders {:?}",
        registration.ide_name,
        registration.pid,
        workspace.port(),
        registration.workspace_folders
    );
    while let Some(event) = workspace.next_event().await {
        match event {
            WorkspaceEvent::Notification { method, .. } => {
                debug!(
                    "{} (pid {}): {method}",
                    registration.ide_name, registration.pid
                );
            }
            WorkspaceEvent::MalformedNotification { method, error } => warn!(
                "{} (pid {}) sent a {method} that is not valid, and it was not passed on: {error}",
                registration.ide_name, registration.pid
            ),
            // A kind of event that this version does not know.
            _ => {}
        }
    }
    let dropped = workspace.dropped_events();
    if dropped > 0 {
        warn!(
            "{dropped} events of the adapter (pid {}) were not read in time",
            registration.pid
        );
    }
    workspace.close().await;
    info!(
        "adapter gone: {} (pid {})",
        registration.ide_name, registration.pid
    );
    let _ = gone.send(());
}
