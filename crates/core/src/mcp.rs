//! The Claude side of the MCP protocol: what Claude Code's MCP client talks to.
//!
//! The protocol itself (the handshake, `tools/list`, `tools/call`, errors) is handled by the `rmcp` crate, and
//! nothing of `rmcp` shows in this module's interface: tools are described with this module's own types, and what
//! a tool does is left to a [`ToolBackend`]. Replacing `rmcp`, or its next major version, touches this file only.
//!
//! Each connection that a [`McpHub`] is given is served in a task of its own. The hub keeps the connected clients
//! of one workspace, so that a notification (`selection_changed`, ...) can be sent to all of them.

use std::collections::{HashMap, VecDeque};
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CustomNotification,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    ServerNotification, Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Map, Value, json};
use tokio::sync::mpsc;

use crate::ws::Connection;

/// A tool that Claude Code can call.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDefinition {
    /// The name that Claude Code calls it by.
    pub name: String,
    /// What it does, for the model.
    pub description: String,
    /// The JSON schema of its arguments (an object schema).
    pub input_schema: Value,
}

/// What a tool call gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolResult {
    /// The text that Claude gets.
    pub text: String,
    /// More texts, after the first: a result may have several (the answer to `openDiff` has two, `FILE_SAVED` and then
    /// the contents).
    pub more: Vec<String>,
    /// Whether the call failed (the text then says why). A failed call is still an answer to the request.
    pub is_error: bool,
}

/// A call that cannot be answered at all, as opposed to one that fails (see [`ToolResult::is_error`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolError {
    /// There is no tool of this name.
    UnknownTool(String),
}

/// What the tools do. The middle layer implements it; this module only carries the calls.
pub trait ToolBackend: Send + Sync + 'static {
    /// The tools that are available now.
    fn tools(&self) -> Vec<ToolDefinition>;

    /// Runs a tool. `arguments` is a JSON object (empty if the client sent none).
    fn call(
        &self,
        name: &str,
        arguments: Value,
    ) -> impl Future<Output = Result<ToolResult, ToolError>> + Send;
}

/// A connected Claude Code, as far as it has introduced itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientInfo {
    /// `clientInfo.name` of its `initialize` (for Claude Code, `claude-code`).
    pub name: String,
    /// `clientInfo.version`.
    pub version: String,
    /// The process of the Claude Code, from its `ide_connected` notification, once that has arrived.
    pub pid: Option<u32>,
}

/// How many notifications may wait for one client. Past that, the oldest is dropped: a client that is that far behind
/// is not keeping up, and it must not hold up the others, or the workspace.
const OUTBOX: usize = 64;

/// The notifications that wait to be sent to one client, which a task of its own sends one after the other.
///
/// Sending never waits. A notification that is a *state* (the selection) replaces the one of the same method that is
/// still waiting, so that a client that is slow gets the latest state, not the history of it, and the latest is never the
/// one that is lost. The others (a mentioned file) are kept in order, up to [`OUTBOX`].
struct Outbox {
    state: Mutex<OutboxState>,
    wake: tokio::sync::Notify,
}

struct OutboxState {
    queue: VecDeque<(String, Value, bool)>,
    closed: bool,
}

impl Outbox {
    fn new() -> Self {
        Self {
            state: Mutex::new(OutboxState {
                queue: VecDeque::new(),
                closed: false,
            }),
            wake: tokio::sync::Notify::new(),
        }
    }

    fn push(&self, method: &str, params: Value, replaces_waiting: bool) {
        let mut state = self.state.lock().expect("not poisoned");
        if state.closed {
            return;
        }
        if replaces_waiting {
            state
                .queue
                .retain(|(waiting, _, state_like)| !(*state_like && waiting == method));
        }
        if state.queue.len() >= OUTBOX {
            state.queue.pop_front();
        }
        state
            .queue
            .push_back((method.to_owned(), params, replaces_waiting));
        drop(state);
        self.wake.notify_one();
    }

    /// The next one to send; `None` once the outbox is closed and empty.
    async fn next(&self) -> Option<(String, Value)> {
        loop {
            {
                let mut state = self.state.lock().expect("not poisoned");
                if let Some((method, params, _)) = state.queue.pop_front() {
                    return Some((method, params));
                }
                if state.closed {
                    return None;
                }
            }
            self.wake.notified().await;
        }
    }

    fn is_closed(&self) -> bool {
        self.state.lock().expect("not poisoned").closed
    }

    fn close(&self) {
        self.state.lock().expect("not poisoned").closed = true;
        self.wake.notify_one();
    }
}

/// A Claude Code that has come or gone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientChange {
    /// It has completed the handshake. Its `pid` is usually not known yet: it comes with a notification after that.
    Connected(ClientInfo),
    /// It is gone. This is what was known of it.
    Disconnected(ClientInfo),
}

/// Told of every [`ClientChange`], on the task of the connection: it must not block.
pub type Observer = Arc<dyn Fn(ClientChange) + Send + Sync>;

struct ClientEntry {
    outbox: Arc<Outbox>,
    name: String,
    version: String,
    pid: Arc<Mutex<Option<u32>>>,
}

impl ClientEntry {
    fn info(&self) -> ClientInfo {
        ClientInfo {
            name: self.name.clone(),
            version: self.version.clone(),
            pid: *self.pid.lock().expect("not poisoned"),
        }
    }
}

struct HubInner<B> {
    backend: B,
    server_name: String,
    server_version: String,
    handshake_timeout: Duration,
    state_resends: Vec<Duration>,
    observer: Observer,
    /// The last notification of each method that is to reach a client that connects later too.
    remembered: Mutex<HashMap<String, Value>>,
    clients: Mutex<HashMap<u64, ClientEntry>>,
    next_id: AtomicU64,
}

/// The connected clients of one workspace, and the tools they can call.
pub struct McpHub<B> {
    inner: Arc<HubInner<B>>,
}

impl<B> Clone for McpHub<B> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<B> std::fmt::Debug for McpHub<B> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("McpHub").finish_non_exhaustive()
    }
}

impl<B: ToolBackend> McpHub<B> {
    /// A hub whose clients are told that the server is `server_name` / `server_version`. A client that has not
    /// completed the MCP handshake within `handshake_timeout` is disconnected. The states that were remembered (see
    /// [`notify_remembering`](Self::notify_remembering)) are sent to a client again at each of `state_resends`, counted from
    /// its handshake (see [`crate::workspace::Config::claude_state_resends`]). `observer` is told when a client has
    /// connected and when it is gone.
    pub fn new(
        backend: B,
        server_name: impl Into<String>,
        server_version: impl Into<String>,
        handshake_timeout: Duration,
        state_resends: Vec<Duration>,
        observer: Observer,
    ) -> Self {
        Self {
            inner: Arc::new(HubInner {
                backend,
                server_name: server_name.into(),
                server_version: server_version.into(),
                handshake_timeout,
                state_resends,
                observer,
                remembered: Mutex::new(HashMap::new()),
                clients: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(1),
            }),
        }
    }

    /// Serves a connection that has passed the WebSocket upgrade. Must be called inside a tokio runtime; the work is
    /// done in a task that ends when the connection does.
    pub fn attach(&self, connection: Connection) {
        let hub = self.inner.clone();
        tokio::spawn(async move {
            let pid = Arc::new(Mutex::new(None));
            let (ended, gone) = tokio::sync::watch::channel(false);
            let handler = IdeHandler {
                hub: hub.clone(),
                pid: pid.clone(),
                gone,
            };
            // A client that has the token but never speaks would otherwise hold a task and a connection for ever.
            let served = tokio::time::timeout(
                hub.handshake_timeout,
                handler.serve(bridge(connection, ended)),
            )
            .await;
            let Ok(Ok(running)) = served else {
                return;
            };
            let (name, version) = running
                .peer()
                .peer_info()
                .map(|info| {
                    (
                        info.client_info.name.clone(),
                        info.client_info.version.clone(),
                    )
                })
                .unwrap_or_default();
            let id = hub.next_id.fetch_add(1, Ordering::Relaxed);

            let outbox = Arc::new(Outbox::new());
            let peer = running.peer().clone();
            let sending = outbox.clone();
            tokio::spawn(async move {
                while let Some((method, params)) = sending.next().await {
                    let notification = CustomNotification::new(method, Some(params));
                    let sent = peer
                        .send_notification(ServerNotification::CustomNotification(notification))
                        .await;
                    if sent.is_err() {
                        break;
                    }
                }
            });
            let connected = {
                // What a client that connects now has missed (the selection that is current, for one) is queued, and the
                // client is registered, in one step with respect to `notify_remembering` (which also holds the clients
                // while it remembers): otherwise a state that arrives in between would reach every client but this one,
                // and this one would be left with the older.
                let mut clients = hub.clients.lock().expect("not poisoned");
                for (method, params) in hub.remembered.lock().expect("not poisoned").iter() {
                    outbox.push(method, params.clone(), true);
                }
                clients.insert(
                    id,
                    ClientEntry {
                        outbox: outbox.clone(),
                        name,
                        version,
                        pid,
                    },
                );
                clients[&id].info()
            };
            // Told with the clients no longer held: an observer that asks the hub something must not wait for itself.
            (hub.observer)(ClientChange::Connected(connected));
            // Claude Code takes a notification only once it has set up what receives it, which happens some time after the
            // handshake, and not always after the same time (seen with 2.1.322: a notification sent at once is thrown
            // away, and one sent a few tens or a few hundreds of milliseconds later gets through or does not, from one
            // start to the next). So a state that is remembered is sent again, a few times. It is the same state, and what
            // waits in the queue is replaced by it.
            if !hub.state_resends.is_empty() {
                let resending = hub.clone();
                let queue = outbox.clone();
                tokio::spawn(async move {
                    let start = tokio::time::Instant::now();
                    for after in resending.state_resends.iter() {
                        tokio::time::sleep_until(start + *after).await;
                        if queue.is_closed() {
                            return;
                        }
                        // With the clients held, as `notify_remembering` holds them: a state that arrives now is queued
                        // after this one, not before it.
                        let _clients = resending.clients.lock().expect("not poisoned");
                        for (method, params) in
                            resending.remembered.lock().expect("not poisoned").iter()
                        {
                            queue.push(method, params.clone(), true);
                        }
                    }
                });
            }
            let _ = running.waiting().await;
            // The entry goes, and the outbox is closed, which ends the task that sends from it.
            let gone = hub.clients.lock().expect("not poisoned").remove(&id);
            outbox.close();
            if let Some(entry) = gone {
                (hub.observer)(ClientChange::Disconnected(entry.info()));
            }
        });
    }

    /// The clients that have completed their handshake and are still connected.
    pub fn clients(&self) -> Vec<ClientInfo> {
        let clients = self.inner.clients.lock().expect("not poisoned");
        let mut infos: Vec<(u64, ClientInfo)> = clients
            .iter()
            .map(|(id, entry)| (*id, entry.info()))
            .collect();
        infos.sort_by_key(|(id, _)| *id);
        infos.into_iter().map(|(_, info)| info).collect()
    }

    /// Like [`notify`](Self::notify), for a *state* such as the selection: the notification is kept, so that a client that
    /// connects later is sent it too (only the last one of each method is kept), and for a client that is slow it
    /// replaces the one of the same method that is still waiting.
    pub fn notify_remembering(&self, method: &str, params: Value) {
        // The clients are held while the state is remembered and queued, so that a client that is being attached sees
        // either all of this or none of it (see `attach`). The order of the locks is always clients, then remembered.
        let clients = self.inner.clients.lock().expect("not poisoned");
        self.inner
            .remembered
            .lock()
            .expect("not poisoned")
            .insert(method.to_owned(), params.clone());
        for entry in clients.values() {
            entry.outbox.push(method, params.clone(), true);
        }
    }

    /// Sends a notification (a method of this protocol that MCP does not define, such as `selection_changed`) to every
    /// connected client. A client that cannot be reached is skipped.
    ///
    /// This never waits for a client: the notification is put in the client's queue (the oldest one is dropped if the
    /// queue is full: a client that is that far behind is not keeping up). Notifications for one client stay in order.
    pub fn notify(&self, method: &str, params: Value) {
        self.broadcast(method, params, false);
    }

    fn broadcast(&self, method: &str, params: Value, state_like: bool) {
        for entry in self.inner.clients.lock().expect("not poisoned").values() {
            entry.outbox.push(method, params.clone(), state_like);
        }
    }
}

/// The handler of one client's connection.
struct IdeHandler<B> {
    hub: Arc<HubInner<B>>,
    pid: Arc<Mutex<Option<u32>>>,
    /// Becomes true when the connection has ended. `rmcp` lets the handlers that are running finish for up to five seconds
    /// after that, and then cancels them; a call that holds something (a diff on the screen) should not wait for that.
    gone: tokio::sync::watch::Receiver<bool>,
}

impl<B: ToolBackend> ServerHandler for IdeHandler<B> {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build()).with_server_info(
            Implementation::new(&self.hub.server_name, &self.hub.server_version),
        )
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let tools = self
            .hub
            .backend
            .tools()
            .into_iter()
            .map(|tool| {
                let schema = match tool.input_schema {
                    Value::Object(schema) => schema,
                    _ => Map::from_iter([("type".to_owned(), json!("object"))]),
                };
                Tool::new(tool.name, tool.description, Arc::new(schema))
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        // A call that waits for the user (`openDiff`) outlives its client unless it is dropped when the client cancels it or
        // goes away: what the call holds (a diff on the screen) is released when the call is dropped.
        let mut gone = self.gone.clone();
        let called = tokio::select! {
            called = self.hub.backend.call(&request.name, arguments) => called,
            () = context.ct.cancelled() => {
                return Err(McpError::internal_error("the call was cancelled", None));
            }
            _ = gone.wait_for(|gone| *gone) => {
                return Err(McpError::internal_error("the connection has ended", None));
            }
        };
        match called {
            Ok(result) => {
                let mut texts = vec![ContentBlock::text(result.text)];
                texts.extend(result.more.into_iter().map(ContentBlock::text));
                Ok(if result.is_error {
                    CallToolResult::error(texts)
                } else {
                    CallToolResult::success(texts)
                }
                .into())
            }
            Err(ToolError::UnknownTool(name)) => Err(McpError::invalid_params(
                format!("unknown tool: {name}"),
                None,
            )),
        }
    }

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _context: NotificationContext<RoleServer>,
    ) {
        if notification.method == "ide_connected" {
            let pid = notification
                .params
                .as_ref()
                .and_then(|params| params.get("pid"))
                .and_then(Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok());
            *self.pid.lock().expect("not poisoned") = pid;
        }
    }
}

/// What `rmcp` needs of a transport: a `send` whose future is `'static` and may run concurrently, and a `receive`.
/// The [`Connection`] is used through `&mut self`, so a task owns it, and the transport talks to that task through
/// two channels.
struct WsTransport {
    outgoing: mpsc::Sender<String>,
    incoming: mpsc::Receiver<String>,
}

fn bridge(mut connection: Connection, ended: tokio::sync::watch::Sender<bool>) -> WsTransport {
    let (outgoing, mut to_send) = mpsc::channel::<String>(64);
    let (received, incoming) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                text = to_send.recv() => {
                    let Some(text) = text else { break };
                    if connection.send(text).await.is_err() {
                        break;
                    }
                }
                message = connection.recv() => {
                    match message {
                        Some(Ok(text)) => {
                            if received.send(text).await.is_err() {
                                break;
                            }
                        }
                        // A malformed frame, or the end of the connection.
                        _ => break,
                    }
                }
            }
        }
        // However it ended, say so to the calls that wait, and to the peer (a no-op if the connection is already closed).
        let _ = ended.send(true);
        connection.close(1000, "").await;
    });
    WsTransport { outgoing, incoming }
}

impl Transport<RoleServer> for WsTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let outgoing = self.outgoing.clone();
        async move {
            let text = serde_json::to_string(&item).map_err(std::io::Error::other)?;
            outgoing
                .send(text)
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }

    async fn receive(&mut self) -> Option<RxJsonRpcMessage<RoleServer>> {
        loop {
            let text = self.incoming.recv().await?;
            // A message that is not valid JSON-RPC is skipped, not fatal to the connection.
            if let Ok(message) = serde_json::from_str::<RxJsonRpcMessage<RoleServer>>(&text) {
                return Some(message);
            }
        }
    }

    async fn close(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }
}
