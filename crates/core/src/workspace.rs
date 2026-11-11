//! Workspaces: the middle layer. It turns each adapter that registers into a workspace with its own WebSocket port
//! and lock file, and ties their lifetimes to the adapter's.
//!
//! The adapter side (`adapter`) knows nothing of ports and lock files, and the Claude side (`ws`, `lock`) knows
//! nothing of adapters; this module is where they meet.

use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::adapter::{
    self, AdapterHandle, AdapterServer, AdapterSession, Listener, Notification, Registering,
    Registration,
};
use crate::lock::{self, EnvVars, LockContents, LockDirs, LockFile};
use crate::ws::{self, Connection, WsServer};

/// Where the adapter socket goes by default: `adapter.sock` in the runtime directory (see [`lock::runtime_dir`]).
pub fn socket_path(env: &EnvVars) -> PathBuf {
    lock::runtime_dir(env).join("adapter.sock")
}

/// Settings of the workspaces.
#[derive(Clone)]
pub struct Config {
    /// Where the lock files and their records go.
    pub lock_dirs: LockDirs,
    /// The process of this server (see [`LockFile::create`]).
    pub server_pid: u32,
    /// Limits of the WebSocket servers that are started for the workspaces.
    pub ws: ws::Config,
    /// Whether a process is alive; decides which leftovers of an earlier server are cleaned up at start.
    pub is_alive: Arc<dyn Fn(u32) -> bool + Send + Sync>,
}

impl Config {
    /// The usual limits and the real check of processes, for the given directories and server process.
    pub fn new(lock_dirs: LockDirs, server_pid: u32) -> Self {
        Self {
            lock_dirs,
            server_pid,
            ws: ws::Config::default(),
            is_alive: Arc::new(lock::process_is_alive),
        }
    }

    /// The same, with another check of whether a process is alive (for tests).
    pub fn with_liveness(mut self, is_alive: impl Fn(u32) -> bool + Send + Sync + 'static) -> Self {
        self.is_alive = Arc::new(is_alive);
        self
    }
}

impl std::fmt::Debug for Config {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Config")
            .field("lock_dirs", &self.lock_dirs)
            .field("server_pid", &self.server_pid)
            .field("ws", &self.ws)
            .finish_non_exhaustive()
    }
}

/// What happens in a workspace, in the order it happens.
#[derive(Debug)]
pub enum WorkspaceEvent {
    /// A notification from the adapter (`selectionChanged`, `diagnosticsChanged`, ...). `workspaceFoldersChanged`
    /// has already been applied to the lock file when it arrives here.
    Notification { method: String, params: Value },
    /// Claude Code connected to the workspace's port, with the right token.
    ClaudeConnected(Connection),
}

/// The part of a workspace that its task and the [`Workspace`] share.
#[derive(Debug)]
struct Shared {
    folders: Mutex<Vec<String>>,
}

/// A registered workspace. While it lives, its port listens and its lock file exists (until the adapter goes away).
///
/// Dropping it, or [`close`](Workspace::close), tells the adapter `shutdown` and removes the lock file and the port.
#[derive(Debug)]
pub struct Workspace {
    registration: Registration,
    port: u16,
    shared: Arc<Shared>,
    handle: AdapterHandle,
    events: mpsc::Receiver<WorkspaceEvent>,
    finished: oneshot::Receiver<()>,
}

impl Workspace {
    /// What the adapter registered, with the folders as they are now.
    pub fn registration(&self) -> Registration {
        let mut registration = self.registration.clone();
        registration.workspace_folders = self.shared.folders.lock().expect("not poisoned").clone();
        registration
    }

    /// The port on which Claude Code connects.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// A way to send requests and notifications to the adapter.
    pub fn handle(&self) -> AdapterHandle {
        self.handle.clone()
    }

    /// The next event. `None` once the adapter is gone and everything has been cleaned up.
    pub async fn next_event(&mut self) -> Option<WorkspaceEvent> {
        self.events.recv().await
    }

    /// Ends the workspace and waits until its lock file and port are gone.
    pub async fn close(self) {
        let Self {
            events,
            finished,
            handle,
            ..
        } = self;
        drop(events);
        drop(handle);
        let _ = finished.await;
    }
}

/// Accepts adapters on a listener and turns each registration into a [`Workspace`].
#[derive(Debug)]
pub struct WorkspaceServer {
    workspaces: mpsc::Receiver<Workspace>,
    task: JoinHandle<()>,
    stale_removed: Vec<PathBuf>,
    // Raised when the server stops: every workspace says shutdown to its adapter and cleans up.
    stop: watch::Sender<bool>,
    // Each workspace task holds a sender; `shutdown` waits until all of them are gone.
    alive: Option<mpsc::Sender<()>>,
    all_done: mpsc::Receiver<()>,
}

impl WorkspaceServer {
    /// Starts listening for adapters. Must be called inside a tokio runtime.
    ///
    /// First removes the lock files that an earlier server left behind when it died (see [`lock::cleanup_stale`]).
    /// That is done as well as it can be: a failure to look is not a reason not to start.
    pub fn start<L: Listener>(
        listener: L,
        adapter_config: adapter::Config,
        config: Config,
    ) -> Self {
        let is_alive = config.is_alive.clone();
        let stale_removed =
            lock::cleanup_stale(&config.lock_dirs, |pid| is_alive(pid)).unwrap_or_default();
        let adapters = AdapterServer::start(listener, adapter_config);
        let (tx, workspaces) = mpsc::channel(16);
        let (stop, stop_signal) = watch::channel(false);
        let (alive, all_done) = mpsc::channel(1);
        let task = tokio::spawn(provision_loop(
            adapters,
            config,
            tx,
            stop_signal,
            alive.clone(),
        ));
        Self {
            workspaces,
            task,
            stale_removed,
            stop,
            alive: Some(alive),
            all_done,
        }
    }

    /// Stops the server: no more adapters are accepted, every workspace tells its adapter `shutdown`, and all lock
    /// files and ports are gone when this returns. Dropping the server does the same, without waiting.
    pub async fn shutdown(mut self) {
        self.task.abort();
        let _ = self.stop.send(true);
        self.alive = None;
        // Ends when the last workspace task has dropped its sender.
        while self.all_done.recv().await.is_some() {}
    }

    /// The lock files of an earlier server that were removed when this one started.
    pub fn stale_removed(&self) -> &[PathBuf] {
        &self.stale_removed
    }

    /// The next adapter that has registered and been set up. `None` only if the listener is closed.
    pub async fn accept(&mut self) -> Option<Workspace> {
        self.workspaces.recv().await
    }
}

impl Drop for WorkspaceServer {
    fn drop(&mut self) {
        self.task.abort();
        let _ = self.stop.send(true);
    }
}

async fn provision_loop(
    mut adapters: AdapterServer,
    config: Config,
    workspaces: mpsc::Sender<Workspace>,
    stop: watch::Receiver<bool>,
    alive: mpsc::Sender<()>,
) {
    while let Some(registering) = adapters.accept().await {
        // Each registration is set up in a task of its own, so that a slow one holds up no other.
        tokio::spawn(provision(
            registering,
            config.clone(),
            workspaces.clone(),
            stop.clone(),
            alive.clone(),
        ));
    }
}

async fn provision(
    registering: Registering,
    config: Config,
    workspaces: mpsc::Sender<Workspace>,
    stop: watch::Receiver<bool>,
    alive: mpsc::Sender<()>,
) {
    let registration = registering.registration().clone();
    let (ws_server, lock) = match set_up(&config, &registration).await {
        Ok(parts) => parts,
        Err(error) => {
            registering.reject(format!("could not set up the workspace: {error}"));
            return;
        }
    };
    let port = ws_server.port();
    let session = registering.accept(port);

    let shared = Arc::new(Shared {
        folders: Mutex::new(registration.workspace_folders.clone()),
    });
    let (events_tx, events) = mpsc::channel(256);
    let (finished_tx, finished) = oneshot::channel();
    let workspace = Workspace {
        registration,
        port,
        shared: shared.clone(),
        handle: session.handle(),
        events,
        finished,
    };
    tokio::spawn(run(Running {
        session,
        ws_server,
        lock,
        shared,
        events: events_tx,
        finished: finished_tx,
        stop,
        alive,
    }));
    // If nobody is accepting workspaces any more, the workspace is dropped here and its task cleans up.
    let _ = workspaces.send(workspace).await;
}

async fn set_up(config: &Config, registration: &Registration) -> io::Result<(WsServer, LockFile)> {
    let token = lock::generate_token()?;
    let ws_server = WsServer::start(token.clone(), config.ws.clone()).await?;
    let contents = LockContents {
        pid: registration.pid,
        workspace_folders: registration.workspace_folders.clone(),
        ide_name: registration.ide_name.clone(),
        transport: lock::TRANSPORT_WS.to_owned(),
        running_in_windows: false,
        auth_token: token,
    };
    let lock = LockFile::create(
        &config.lock_dirs,
        ws_server.port(),
        contents,
        config.server_pid,
    )?;
    Ok((ws_server, lock))
}

/// Everything that the task of one workspace owns.
struct Running {
    session: AdapterSession,
    ws_server: WsServer,
    lock: LockFile,
    shared: Arc<Shared>,
    events: mpsc::Sender<WorkspaceEvent>,
    finished: oneshot::Sender<()>,
    stop: watch::Receiver<bool>,
    // Held until the end, so that `WorkspaceServer::shutdown` can tell when every workspace is done.
    alive: mpsc::Sender<()>,
}

/// Serves one workspace until the adapter goes away, the [`Workspace`] is dropped or the server stops, then cleans
/// up.
async fn run(running: Running) {
    let Running {
        mut session,
        mut ws_server,
        mut lock,
        shared,
        events,
        finished,
        mut stop,
        alive: _alive,
    } = running;
    // Whether the owner of the `Workspace`, or the server, ended it, as opposed to the adapter going away.
    let mut owner_closed = false;
    loop {
        tokio::select! {
            notification = session.next_notification() => {
                let Some(Notification { method, params }) = notification else { break };
                if method == "workspaceFoldersChanged" {
                    apply_folders_changed(&params, &mut lock, &shared);
                }
                if events.send(WorkspaceEvent::Notification { method, params }).await.is_err() {
                    owner_closed = true;
                    break;
                }
            }
            connection = ws_server.accept() => {
                let Some(connection) = connection else { break };
                if events.send(WorkspaceEvent::ClaudeConnected(connection)).await.is_err() {
                    owner_closed = true;
                    break;
                }
            }
            () = events.closed() => {
                owner_closed = true;
                break;
            }
            // The server stops (the value was raised, or the server was dropped).
            _ = stop.changed() => {
                owner_closed = true;
                break;
            }
        }
    }
    // The lock file goes first, so that no new CLI finds the workspace while it is being closed.
    let _ = lock.close();
    if owner_closed {
        session.close().await;
    } else {
        drop(session);
    }
    drop(ws_server);
    // Only now does the event stream end, and `close` return.
    drop(events);
    let _ = finished.send(());
}

fn apply_folders_changed(params: &Value, lock: &mut LockFile, shared: &Shared) {
    let Some(folders) = params
        .get("workspaceFolders")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
        })
        .filter(|folders| !folders.is_empty())
    else {
        return;
    };
    if lock.set_workspace_folders(folders.clone()).is_ok() {
        *shared.folders.lock().expect("not poisoned") = folders;
    }
}
