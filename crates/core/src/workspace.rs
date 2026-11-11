//! Workspaces: the middle layer. It turns each adapter that registers into a workspace with its own WebSocket port
//! and lock file, and ties their lifetimes to the adapter's.
//!
//! The adapter side (`adapter`) knows nothing of ports and lock files, and the Claude side (`ws`, `lock`) knows
//! nothing of adapters; this module is where they meet.

use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::task::JoinHandle;

use crate::adapter::{
    self, AdapterHandle, AdapterServer, AdapterSession, Listener, Notification, Registering,
    Registration,
};
use crate::lock::{self, EnvVars, LockContents, LockDirs, LockFile};
use crate::mcp::{ClientChange, ClientInfo, McpHub};
use crate::model::ModelError;
use crate::notifications::{Forward, forward};
use crate::tools::WorkspaceTools;
use crate::ws::{self, WsServer};

/// Where the adapter socket goes by default: `adapter.sock` in the runtime directory (see [`lock::runtime_dir`]).
#[cfg(unix)]
pub fn socket_path(env: &EnvVars) -> PathBuf {
    lock::runtime_dir(env).join("adapter.sock")
}

/// Where the adapters connect by default: the named pipe `\\.\pipe\claude-ide-server-<sid>`, with the security identifier of
/// the user in its name, so that two users of one machine do not meet (a pipe has no directory to keep them apart).
#[cfg(windows)]
pub fn socket_path(env: &EnvVars) -> PathBuf {
    PathBuf::from(claude_ide_windows::default_pipe_name(&env.user))
}

/// See [`Config::claude_observer`].
pub type ClaudeObserver = Arc<dyn Fn(&Registration, ClientChange) + Send + Sync>;

/// Settings of the workspaces.
#[derive(Clone)]
pub struct Config {
    /// Where the lock files and their records go.
    pub lock_dirs: LockDirs,
    /// The process of this server (see [`LockFile::create`]).
    pub server_pid: u32,
    /// Limits of the WebSocket servers that are started for the workspaces.
    pub ws: ws::Config,
    /// How long a Claude Code that has connected may take to complete the MCP handshake.
    pub claude_handshake_timeout: Duration,
    /// When, counted from its connection, the selection that was last reported is sent to a Claude Code again. Claude
    /// Code sets up what receives notifications some time after it is connected, and throws away those that come before;
    /// how long that takes varies from one start to the next. The VS Code extension sends the selection once, 500 ms
    /// after a connection, and that is one of these; the others are for a Claude Code that is slower (ten runs out of ten
    /// reached Claude Code with 250 ms, 1 s and 3 s). Empty for none.
    pub claude_state_resends: Vec<Duration>,
    /// Whether a process is alive; decides which leftovers of an earlier server are cleaned up at start.
    pub is_alive: Arc<dyn Fn(u32) -> bool + Send + Sync>,
    /// Told when a Claude Code connects to a workspace and when it goes, with the registration of the workspace. It
    /// runs on the task of the connection and must not block.
    pub claude_observer: Option<ClaudeObserver>,
    // Where the token of each workspace comes from. The operating system's random source, and nothing else in a normal
    // build: the token is the only thing that keeps other processes of the machine out, so it is not for callers to
    // replace. Tests can, through the `test-support` feature.
    token_source: Arc<dyn Fn() -> io::Result<String> + Send + Sync>,
}

impl Config {
    /// The usual limits and the real check of processes, for the given directories and server process.
    pub fn new(lock_dirs: LockDirs, server_pid: u32) -> Self {
        Self {
            lock_dirs,
            server_pid,
            ws: ws::Config::default(),
            claude_handshake_timeout: Duration::from_secs(10),
            claude_state_resends: vec![
                Duration::from_millis(250),
                Duration::from_millis(500),
                Duration::from_millis(1000),
                Duration::from_millis(3000),
            ],
            is_alive: Arc::new(lock::process_is_alive),
            claude_observer: None,
            token_source: Arc::new(lock::generate_token),
        }
    }

    /// The same, with another source of tokens (to test a failing source, or to know the token in advance).
    ///
    /// Only with the `test-support` feature, which is on only for the tests of this crate.
    #[cfg(feature = "test-support")]
    pub fn with_token_source(
        mut self,
        source: impl Fn() -> io::Result<String> + Send + Sync + 'static,
    ) -> Self {
        self.token_source = Arc::new(source);
        self
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
            .field("claude_handshake_timeout", &self.claude_handshake_timeout)
            .field("claude_state_resends", &self.claude_state_resends)
            .finish_non_exhaustive()
    }
}

/// What happens in a workspace, in the order it happens.
#[derive(Debug)]
#[non_exhaustive]
pub enum WorkspaceEvent {
    /// A notification from the adapter (`selectionChanged`, `diagnosticsChanged`, ...). `workspaceFoldersChanged`
    /// has already been applied to the lock file, and `selectionChanged` and `atMention` have already been passed on
    /// to Claude Code, when it arrives here.
    Notification { method: String, params: Value },
    /// A notification from the adapter that is not what it should be (a missing field, a wrong type); it was not passed
    /// on. The adapter has a bug.
    MalformedNotification { method: String, error: ModelError },
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
    claude: McpHub<WorkspaceTools>,
    events: mpsc::Receiver<WorkspaceEvent>,
    /// How many events were dropped because the owner was too far behind.
    dropped: Arc<AtomicU64>,
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

    /// The Claude Code clients that are connected to this workspace and have completed their handshake.
    pub fn claude_clients(&self) -> Vec<ClientInfo> {
        self.claude.clients()
    }

    /// Sends a notification (`selection_changed`, `at_mentioned`, `diagnostics_changed`, ...) to every connected Claude
    /// Code.
    pub fn notify_claude(&self, method: &str, params: Value) {
        self.claude.notify(method, params);
    }

    /// The next event. `None` once the adapter is gone and everything has been cleaned up.
    ///
    /// The events are information; the workspace never waits for whoever reads them. If more than 256 are waiting, the
    /// newest are dropped (see [`dropped_events`](Self::dropped_events)).
    pub async fn next_event(&mut self) -> Option<WorkspaceEvent> {
        self.events.recv().await
    }

    /// How many events were dropped because they were not read fast enough.
    pub fn dropped_events(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
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
    let dropped = Arc::new(AtomicU64::new(0));
    let (finished_tx, finished) = oneshot::channel();
    let claude = McpHub::new(
        WorkspaceTools::new(registration.methods.clone(), session.handle(), {
            let shared = shared.clone();
            Arc::new(move || shared.folders.lock().expect("not poisoned").clone())
        }),
        "claude-ide-server",
        env!("CARGO_PKG_VERSION"),
        config.claude_handshake_timeout,
        config.claude_state_resends.clone(),
        {
            let observer = config.claude_observer.clone();
            let registration = registration.clone();
            Arc::new(move |change| {
                if let Some(observer) = &observer {
                    observer(&registration, change);
                }
            })
        },
    );
    let workspace = Workspace {
        registration,
        port,
        shared: shared.clone(),
        handle: session.handle(),
        claude: claude.clone(),
        events,
        dropped: dropped.clone(),
        finished,
    };
    tokio::spawn(run(Running {
        session,
        ws_server,
        lock,
        shared,
        claude,
        events: events_tx,
        dropped,
        finished: finished_tx,
        stop,
        alive,
    }));
    // If nobody is accepting workspaces any more, the workspace is dropped here and its task cleans up.
    let _ = workspaces.send(workspace).await;
}

async fn set_up(config: &Config, registration: &Registration) -> io::Result<(WsServer, LockFile)> {
    let token = (config.token_source)()?;
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
    claude: McpHub<WorkspaceTools>,
    events: mpsc::Sender<WorkspaceEvent>,
    dropped: Arc<AtomicU64>,
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
        claude,
        events,
        dropped,
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
                match forward(&method, &params) {
                    Forward::Nothing => {}
                    Forward::Notify { method, params, remember: true } => claude.notify_remembering(method, params),
                    Forward::Notify { method, params, remember: false } => claude.notify(method, params),
                    Forward::Malformed(error) => {
                        let event = WorkspaceEvent::MalformedNotification { method: method.clone(), error };
                        if !publish(&events, &dropped, event) {
                            owner_closed = true;
                            break;
                        }
                    }
                }
                if !publish(&events, &dropped, WorkspaceEvent::Notification { method, params }) {
                    owner_closed = true;
                    break;
                }
            }
            connection = ws_server.accept() => {
                let Some(connection) = connection else { break };
                // The MCP handshake and the tool calls are served in a task of their own.
                claude.attach(connection);
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

/// Hands an event to the owner of the workspace without ever waiting for it: the events are information, and a
/// workspace whose owner is slow (or only looks now and then) must go on passing the selection to Claude Code. If the
/// owner is more than the queue behind, the event is dropped and counted. Returns false if the owner is gone.
fn publish(
    events: &mpsc::Sender<WorkspaceEvent>,
    dropped: &AtomicU64,
    event: WorkspaceEvent,
) -> bool {
    match events.try_send(event) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            dropped.fetch_add(1, Ordering::Relaxed);
            true
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
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
