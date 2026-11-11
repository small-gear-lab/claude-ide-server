//! The adapter side: editors connect here, register a workspace, and answer the server's requests.
//!
//! This layer knows the byte stream, the JSON-RPC framing and the `register` exchange, and nothing about what a
//! workspace needs (a port, a lock file): it hands each registration to its caller as a [`Registering`], and the
//! caller says [`accept`](Registering::accept) with a port or [`reject`](Registering::reject). Everything works with
//! any duplex stream that a [`Listener`] accepts: a Unix socket (a named pipe on Windows) in production, an in-memory
//! stream in tests.

use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, BufReader};
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;

use crate::jsonrpc::{self, FrameError, Message, RpcError, code};

/// The error codes of the adapter protocol, beside the ones that JSON-RPC defines.
pub mod error_code {
    /// The adapter speaks a protocol version that the server does not.
    pub const UNSUPPORTED_PROTOCOL_VERSION: i64 = -32000;
    /// The server could not set up the workspace (the port, the lock file).
    pub const SETUP_FAILED: i64 = -32001;
    /// The first message was not `register`, or `register` came twice.
    pub const NOT_REGISTERED: i64 = -32002;
}

/// The version of the adapter protocol that this server speaks. `register` must carry the same number.
pub const PROTOCOL_VERSION: u64 = 1;

/// The largest message that an adapter may send.
const MAX_MESSAGE_BYTES: usize = 16 << 20;

/// Something that accepts connections from adapters.
pub trait Listener: Send + 'static {
    /// The duplex byte stream of one connection.
    type Stream: AsyncRead + AsyncWrite + Unpin + Send + 'static;

    /// Waits for the next connection. An error of kind `BrokenPipe` means that the listener is closed for good.
    fn accept(&mut self) -> impl Future<Output = io::Result<Self::Stream>> + Send;
}

#[cfg(unix)]
pub use unix::UnixListener;

/// What the executable listens on for adapters on this system: a Unix socket, or a named pipe on Windows.
#[cfg(unix)]
pub type NativeListener = UnixListener;

#[cfg(windows)]
pub use pipe::PipeListener;

/// What the executable listens on for adapters on this system: a Unix socket, or a named pipe on Windows.
#[cfg(windows)]
pub type NativeListener = PipeListener;

#[cfg(unix)]
mod unix {
    use std::io;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use super::Listener;
    use crate::fsutil::create_private_dir;

    /// A listener on a Unix domain socket, with the socket file private to the user.
    #[derive(Debug)]
    pub struct UnixListener {
        inner: tokio::net::UnixListener,
        path: Option<PathBuf>,
        /// Whether this listener made the socket file, and so removes it when it is dropped.
        owns_file: bool,
    }

    impl UnixListener {
        /// Binds the socket at `path`, creating its directory with mode 0700 and giving the socket mode 0600.
        ///
        /// A socket file left by a server that died is replaced; one that another server is listening on is an
        /// error.
        pub fn bind(path: impl Into<PathBuf>) -> io::Result<Self> {
            let path = path.into();
            if let Some(parent) = path.parent() {
                create_private_dir(parent)?;
            }
            if path.exists() {
                match std::os::unix::net::UnixStream::connect(&path) {
                    Ok(_) => {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            format!("another server is listening on {}", path.display()),
                        ));
                    }
                    Err(error)
                        if matches!(
                            error.kind(),
                            io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
                        ) =>
                    {
                        std::fs::remove_file(&path)?;
                    }
                    Err(error) => return Err(error),
                }
            }
            let inner = tokio::net::UnixListener::bind(&path)?;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
            Ok(Self {
                inner,
                path: Some(path),
                owns_file: true,
            })
        }

        /// Takes a socket that is already listening, which something else made: a supervisor that starts the server
        /// when the first connection comes (socket activation), for one. Must be called inside a tokio runtime.
        ///
        /// The socket is not this listener's to remove: its file stays when the listener is dropped, so that whatever
        /// made it can hand it to the next server.
        pub fn from_listening(socket: std::os::unix::net::UnixListener) -> io::Result<Self> {
            socket.set_nonblocking(true)?;
            let path = socket
                .local_addr()?
                .as_pathname()
                .map(std::path::Path::to_path_buf);
            Ok(Self {
                inner: tokio::net::UnixListener::from_std(socket)?,
                path,
                owns_file: false,
            })
        }

        /// Where the socket is, if it has a path (a socket that was handed over may not).
        pub fn path(&self) -> Option<&Path> {
            self.path.as_deref()
        }
    }

    impl Listener for UnixListener {
        type Stream = tokio::net::UnixStream;

        async fn accept(&mut self) -> io::Result<Self::Stream> {
            self.inner.accept().await.map(|(stream, _address)| stream)
        }
    }

    impl Drop for UnixListener {
        fn drop(&mut self) {
            if let (true, Some(path)) = (self.owns_file, &self.path) {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

#[cfg(windows)]
mod pipe {
    use std::io;
    use std::path::{Path, PathBuf};

    use tokio::net::windows::named_pipe::NamedPipeServer;

    use super::Listener;

    /// A listener on a named pipe, which only the user of the process can open.
    ///
    /// A pipe has instances, and a client connects to one that is waiting: the listener keeps one waiting, and makes the
    /// next as it hands out the one that was connected.
    #[derive(Debug)]
    pub struct PipeListener {
        name: PathBuf,
        waiting: NamedPipeServer,
    }

    impl PipeListener {
        /// Makes the pipe `name` (`\\.\pipe\<name>`), private to the user.
        ///
        /// One that another server is listening on is an error (a pipe goes with its last handle, so a server that died
        /// leaves nothing behind that would need to be replaced, as a Unix socket file does).
        pub fn bind(name: impl Into<PathBuf>) -> io::Result<Self> {
            let name = name.into();
            match claude_ide_windows::create_private_pipe(name.as_os_str(), true) {
                Ok(waiting) => Ok(Self { name, waiting }),
                // The system says that the name is taken by refusing the first instance of it.
                Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
                    Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("another server is listening on {}", name.display()),
                    ))
                }
                Err(error) => Err(error),
            }
        }

        /// The name of the pipe.
        pub fn path(&self) -> Option<&Path> {
            Some(&self.name)
        }
    }

    impl Listener for PipeListener {
        type Stream = NamedPipeServer;

        async fn accept(&mut self) -> io::Result<NamedPipeServer> {
            self.waiting.connect().await?;
            let next = claude_ide_windows::create_private_pipe(self.name.as_os_str(), false)?;
            Ok(std::mem::replace(&mut self.waiting, next))
        }
    }
}

/// An in-memory listener, for tests and for embedding: [`MemoryConnector::connect`](memory::MemoryConnector::connect) makes the other end.
pub mod memory {
    use super::{Listener, io};
    use tokio::io::DuplexStream;
    use tokio::sync::mpsc;

    /// The accepting side.
    #[derive(Debug)]
    pub struct MemoryListener {
        incoming: mpsc::Receiver<DuplexStream>,
    }

    /// The connecting side. Cheap to clone.
    #[derive(Debug, Clone)]
    pub struct MemoryConnector {
        outgoing: mpsc::Sender<DuplexStream>,
    }

    /// A connected pair of a listener and a connector.
    pub fn listener() -> (MemoryListener, MemoryConnector) {
        let (outgoing, incoming) = mpsc::channel(16);
        (MemoryListener { incoming }, MemoryConnector { outgoing })
    }

    impl MemoryConnector {
        /// Opens a connection; the returned stream is the adapter's end.
        pub async fn connect(&self) -> io::Result<DuplexStream> {
            let (adapter_end, server_end) = tokio::io::duplex(64 * 1024);
            self.outgoing
                .send(server_end)
                .await
                .map_err(|_| io::Error::from(io::ErrorKind::BrokenPipe))?;
            Ok(adapter_end)
        }
    }

    impl Listener for MemoryListener {
        type Stream = DuplexStream;

        async fn accept(&mut self) -> io::Result<DuplexStream> {
            self.incoming
                .recv()
                .await
                .ok_or_else(|| io::Error::from(io::ErrorKind::BrokenPipe))
        }
    }
}

/// Timeouts of the adapter side.
#[derive(Debug, Clone)]
pub struct Config {
    /// How long an adapter may take to send `register`, and how long the answer to it may take.
    pub register_timeout: Duration,
    /// How long [`AdapterHandle::request`] waits for the answer.
    pub request_timeout: Duration,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            register_timeout: Duration::from_secs(10),
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// What an adapter says about itself in `register`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Registration {
    /// The name shown in `/ide`.
    pub ide_name: String,
    /// The process of the adapter.
    pub pid: u32,
    /// The folders of the workspace.
    pub workspace_folders: Vec<String>,
    /// The server-to-adapter methods that the adapter implements.
    pub methods: Vec<String>,
}

fn parse_registration(params: &Value) -> Result<Registration, RpcError> {
    let invalid = |what: &str| RpcError::new(code::INVALID_PARAMS, what);
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid("protocolVersion must be an integer"))?;
    if version != PROTOCOL_VERSION {
        return Err(RpcError::new(
            error_code::UNSUPPORTED_PROTOCOL_VERSION,
            format!(
                "protocol version {version} is not supported; this server speaks {PROTOCOL_VERSION}"
            ),
        ));
    }
    let ide_name = params
        .get("ideName")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("ideName must be a non-empty string"))?
        .to_owned();
    let pid = params
        .get("pid")
        .and_then(Value::as_u64)
        .and_then(|pid| u32::try_from(pid).ok())
        .filter(|pid| *pid > 0)
        .ok_or_else(|| invalid("pid must be a positive integer"))?;
    let strings = |name: &str| -> Result<Vec<String>, RpcError> {
        params
            .get(name)
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()
            })
            .ok_or_else(|| invalid(&format!("{name} must be an array of strings")))
    };
    let workspace_folders = strings("workspaceFolders")?;
    if workspace_folders.is_empty() {
        return Err(invalid("workspaceFolders must not be empty"));
    }
    let methods = strings("methods")?;
    Ok(Registration {
        ide_name,
        pid,
        workspace_folders,
        methods,
    })
}

/// A notification from the adapter (`selectionChanged`, `diagnosticsChanged`, `workspaceFoldersChanged`, ...).
#[derive(Debug, Clone, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// Why a request to an adapter has no answer.
#[derive(Debug, Clone, PartialEq)]
pub enum RequestError {
    /// The adapter is gone, or the session was closed.
    Disconnected,
    /// No answer within the request timeout.
    Timeout,
    /// The adapter did not declare this method in `register`, so it is not called.
    NotDeclared(String),
    /// The adapter answered with an error.
    Rpc(RpcError),
}

impl std::fmt::Display for RequestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Disconnected => f.write_str("the adapter is disconnected"),
            Self::Timeout => f.write_str("the adapter did not answer in time"),
            Self::NotDeclared(method) => write!(f, "the adapter does not implement {method}"),
            Self::Rpc(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for RequestError {}

/// The requests that wait for an answer, by the id that they were sent with. The handle puts an entry in before it
/// sends and takes it out when it stops waiting; the session task takes it out when the answer arrives.
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

/// Takes a request out of the pending ones when its caller stops waiting, whichever way that happens (an answer, a
/// timeout, or the caller's future being dropped), so that no entry is left behind.
struct Waiting {
    pending: Pending,
    id: u64,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.pending.lock().expect("not poisoned").remove(&self.id);
    }
}

enum Command {
    Request {
        id: u64,
        method: String,
        params: Value,
    },
    Notify {
        method: String,
        params: Value,
    },
}

/// A way to send requests and notifications to one adapter. Cheap to clone.
#[derive(Clone)]
pub struct AdapterHandle {
    commands: mpsc::Sender<Command>,
    request_timeout: Duration,
    methods: Arc<Vec<String>>,
    pending: Pending,
    next_id: Arc<AtomicU64>,
}

impl std::fmt::Debug for AdapterHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdapterHandle")
            .field("methods", &self.methods)
            .finish_non_exhaustive()
    }
}

impl AdapterHandle {
    /// Sends a request and waits for the answer (at most the request timeout). Only methods that the adapter
    /// declared in `register` are called.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RequestError> {
        self.request_with(method, params, Some(self.request_timeout))
            .await
    }

    /// As [`request`](Self::request), with another time to wait: `None` waits until the adapter answers or is gone
    /// (for a request that waits for the user, such as `openDiff`).
    pub async fn request_with(
        &self,
        method: &str,
        params: Value,
        timeout: Option<Duration>,
    ) -> Result<Value, RequestError> {
        if !self.methods.iter().any(|declared| declared == method) {
            return Err(RequestError::NotDeclared(method.to_owned()));
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (reply, answer) = oneshot::channel();
        self.pending.lock().expect("not poisoned").insert(id, reply);
        let _waiting = Waiting {
            pending: self.pending.clone(),
            id,
        };
        self.commands
            .send(Command::Request {
                id,
                method: method.to_owned(),
                params,
            })
            .await
            .map_err(|_| RequestError::Disconnected)?;
        let answer = match timeout {
            Some(limit) => match tokio::time::timeout(limit, answer).await {
                Ok(answer) => answer,
                Err(_) => return Err(RequestError::Timeout),
            },
            None => answer.await,
        };
        match answer {
            Err(_) => Err(RequestError::Disconnected),
            Ok(Ok(result)) => Ok(result),
            Ok(Err(error)) => Err(RequestError::Rpc(error)),
        }
    }

    /// How many requests are waiting for an answer. For tests: it shows that a request that is answered, timed out or
    /// abandoned leaves nothing behind.
    #[cfg(feature = "test-support")]
    pub fn pending_requests(&self) -> usize {
        self.pending.lock().expect("not poisoned").len()
    }

    /// Sends a notification.
    pub async fn notify(&self, method: &str, params: Value) -> Result<(), RequestError> {
        self.commands
            .send(Command::Notify {
                method: method.to_owned(),
                params,
            })
            .await
            .map_err(|_| RequestError::Disconnected)
    }
}

/// An adapter that has registered and been accepted: its notifications, and a way to call it.
///
/// Dropping it, or [`close`](AdapterSession::close), says `shutdown` to the adapter and ends the session.
#[derive(Debug)]
pub struct AdapterSession {
    handle: AdapterHandle,
    notifications: mpsc::Receiver<Notification>,
    finished: oneshot::Receiver<()>,
}

impl AdapterSession {
    /// A way to send requests and notifications to the adapter.
    pub fn handle(&self) -> AdapterHandle {
        self.handle.clone()
    }

    /// The next notification. `None` once the adapter is gone.
    pub async fn next_notification(&mut self) -> Option<Notification> {
        self.notifications.recv().await
    }

    /// Tells the adapter `shutdown`, ends the session, and waits until that is done.
    pub async fn close(self) {
        let Self {
            handle,
            notifications,
            finished,
        } = self;
        drop(notifications);
        drop(handle);
        let _ = finished.await;
    }
}

enum Decision {
    Accept(u16),
    Reject(String),
}

/// A `register` that is waiting for an answer. The adapter has the answer only once [`accept`](Self::accept) or
/// [`reject`](Self::reject) is called; dropping it counts as a rejection.
#[derive(Debug)]
pub struct Registering {
    registration: Registration,
    decision: oneshot::Sender<Decision>,
    session: AdapterSession,
}

impl Registering {
    /// What the adapter registered.
    pub fn registration(&self) -> &Registration {
        &self.registration
    }

    /// Accepts the adapter: it is told `port`, and the session starts.
    pub fn accept(self, port: u16) -> AdapterSession {
        let _ = self.decision.send(Decision::Accept(port));
        self.session
    }

    /// Refuses the adapter with a reason (error code `-32001`); the connection is closed.
    pub fn reject(self, reason: impl Into<String>) {
        let _ = self.decision.send(Decision::Reject(reason.into()));
    }
}

impl std::fmt::Debug for Decision {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Decision")
    }
}

/// Accepts adapters and turns each `register` into a [`Registering`].
#[derive(Debug)]
pub struct AdapterServer {
    registrations: mpsc::Receiver<Registering>,
    task: JoinHandle<()>,
}

impl AdapterServer {
    /// Starts accepting connections on `listener`. Must be called inside a tokio runtime.
    pub fn start<L: Listener>(listener: L, config: Config) -> Self {
        let (tx, registrations) = mpsc::channel(16);
        let task = tokio::spawn(accept_loop(listener, config, tx));
        Self {
            registrations,
            task,
        }
    }

    /// The next adapter that has sent a valid `register`. `None` only if the listener is closed.
    pub async fn accept(&mut self) -> Option<Registering> {
        self.registrations.recv().await
    }
}

impl Drop for AdapterServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn accept_loop<L: Listener>(mut listener: L, config: Config, tx: mpsc::Sender<Registering>) {
    loop {
        match listener.accept().await {
            Ok(stream) => {
                let config = config.clone();
                let tx = tx.clone();
                tokio::spawn(session(stream, config, tx));
            }
            Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return,
            // A failed accept must not end the server.
            Err(_) => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
}

type Incoming = Result<Message, FrameError>;

async fn read_loop<R: AsyncRead + Unpin>(read: R, tx: mpsc::Sender<Incoming>) {
    let mut reader = BufReader::new(read);
    loop {
        match jsonrpc::read_message(&mut reader, MAX_MESSAGE_BYTES).await {
            Ok(Some(message)) => {
                if tx.send(Ok(message)).await.is_err() {
                    return;
                }
            }
            Ok(None) => return,
            Err(error) => {
                let _ = tx.send(Err(error)).await;
                return;
            }
        }
    }
}

async fn reply<W: AsyncWrite + Unpin>(
    write: &mut W,
    id: Value,
    outcome: Result<Value, RpcError>,
) -> io::Result<()> {
    jsonrpc::write_message(write, &Message::Response { id, outcome }).await
}

/// One connection, from the first byte to the last: register, wait for the answer, then serve until it ends.
async fn session<S>(stream: S, config: Config, registrations: mpsc::Sender<Registering>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let (read_half, mut write) = tokio::io::split(stream);
    let (incoming_tx, mut incoming) = mpsc::channel::<Incoming>(64);
    let reader = tokio::spawn(read_loop(read_half, incoming_tx));

    let first = tokio::time::timeout(
        config.register_timeout,
        read_register(&mut incoming, &mut write),
    )
    .await;
    let Ok(Some((id, registration))) = first else {
        reader.abort();
        return;
    };

    let (notifications_tx, notifications) = mpsc::channel(256);
    let (commands_tx, commands) = mpsc::channel(64);
    let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
    let (finished_tx, finished) = oneshot::channel();
    let (decision_tx, decision_rx) = oneshot::channel();
    let registering = Registering {
        registration: registration.clone(),
        decision: decision_tx,
        session: AdapterSession {
            handle: AdapterHandle {
                commands: commands_tx,
                request_timeout: config.request_timeout,
                methods: Arc::new(registration.methods.clone()),
                pending: pending.clone(),
                next_id: Arc::new(AtomicU64::new(1)),
            },
            notifications,
            finished,
        },
    };
    if registrations.send(registering).await.is_err() {
        reader.abort();
        return;
    }

    let decision = tokio::time::timeout(
        config.register_timeout,
        wait_for_decision(decision_rx, &mut incoming),
    )
    .await;
    let port = match decision {
        Ok(Some(Decision::Accept(port))) => port,
        Ok(Some(Decision::Reject(reason))) => {
            let error = RpcError::new(error_code::SETUP_FAILED, reason);
            let _ = reply(&mut write, id, Err(error)).await;
            reader.abort();
            return;
        }
        // The adapter went away, or nobody answered in time.
        Ok(None) | Err(_) => {
            reader.abort();
            return;
        }
    };
    if reply(&mut write, id, Ok(json!({ "port": port })))
        .await
        .is_err()
    {
        reader.abort();
        return;
    }

    serve(
        &mut incoming,
        &mut write,
        commands,
        notifications_tx,
        &pending,
    )
    .await;

    reader.abort();
    let _ = finished_tx.send(());
}

/// Waits for the first message, which must be `register`. `None` ends the connection.
async fn read_register<W: AsyncWrite + Unpin>(
    incoming: &mut mpsc::Receiver<Incoming>,
    write: &mut W,
) -> Option<(Value, Registration)> {
    let (id, params) = match incoming.recv().await? {
        Ok(Message::Request { id, method, params }) if method == "register" => (id, params),
        Ok(Message::Request { id, .. }) => {
            let error = RpcError::new(
                error_code::NOT_REGISTERED,
                "the first message must be register",
            );
            let _ = reply(write, id, Err(error)).await;
            return None;
        }
        // A notification or a response cannot be answered; the connection is not an adapter.
        _ => return None,
    };
    match parse_registration(&params) {
        Ok(registration) => Some((id, registration)),
        Err(error) => {
            let _ = reply(write, id, Err(error)).await;
            None
        }
    }
}

/// Waits for the caller's decision, while noticing an adapter that goes away. Whatever the adapter sends before it
/// has its answer is ignored.
async fn wait_for_decision(
    mut decision: oneshot::Receiver<Decision>,
    incoming: &mut mpsc::Receiver<Incoming>,
) -> Option<Decision> {
    loop {
        tokio::select! {
            decided = &mut decision => return decided.ok(),
            message = incoming.recv() => {
                if !matches!(message, Some(Ok(_))) {
                    return None;
                }
            }
        }
    }
}

/// Serves a registered adapter until it goes away or the [`AdapterSession`] is dropped.
async fn serve<W: AsyncWrite + Unpin>(
    incoming: &mut mpsc::Receiver<Incoming>,
    write: &mut W,
    mut commands: mpsc::Receiver<Command>,
    notifications: mpsc::Sender<Notification>,
    pending: &Pending,
) {
    // Set when the session is dropped (its handle and its receiver go together), as opposed to the adapter going
    // away: only then is there someone to say goodbye to.
    let mut say_goodbye = false;
    loop {
        tokio::select! {
            message = incoming.recv() => {
                let Some(Ok(message)) = message else { break };
                match message {
                    Message::Response { id, outcome } => {
                        let waiting = id.as_u64().and_then(|id| pending.lock().expect("not poisoned").remove(&id));
                        if let Some(waiting) = waiting {
                            let _ = waiting.send(outcome);
                        }
                    }
                    Message::Notification { method, params } => {
                        if notifications.send(Notification { method, params }).await.is_err() {
                            say_goodbye = true;
                            break;
                        }
                    }
                    Message::Request { id, method, .. } => {
                        let error = if method == "register" {
                            RpcError::new(error_code::NOT_REGISTERED, "already registered")
                        } else {
                            RpcError::new(code::METHOD_NOT_FOUND, format!("the server has no method {method}"))
                        };
                        if reply(write, id, Err(error)).await.is_err() {
                            break;
                        }
                    }
                }
            }
            command = commands.recv() => {
                let Some(command) = command else {
                    say_goodbye = true;
                    break;
                };
                let written = match command {
                    Command::Request { id, method, params } => {
                        jsonrpc::write_message(write, &Message::Request { id: json!(id), method, params }).await
                    }
                    Command::Notify { method, params } => {
                        jsonrpc::write_message(write, &Message::Notification { method, params }).await
                    }
                };
                if written.is_err() {
                    break;
                }
            }
            () = notifications.closed() => {
                say_goodbye = true;
                break;
            }
        }
    }
    if say_goodbye {
        let goodbye = Message::Notification {
            method: "shutdown".to_owned(),
            params: json!({}),
        };
        let _ = tokio::time::timeout(
            Duration::from_secs(1),
            jsonrpc::write_message(write, &goodbye),
        )
        .await;
    }
    // Pending requests are failed by dropping their senders.
    pending.lock().expect("not poisoned").clear();
}
