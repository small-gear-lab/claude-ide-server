//! The Claude side of the MCP protocol: what Claude Code's MCP client talks to.
//!
//! The protocol itself (the handshake, `tools/list`, `tools/call`, errors) is handled by the `rmcp` crate, and
//! nothing of `rmcp` shows in this module's interface: tools are described with this module's own types, and what
//! a tool does is left to a [`ToolBackend`]. Replacing `rmcp`, or its next major version, touches this file only.
//!
//! Each connection that a [`McpHub`] is given is served in a task of its own. The hub keeps the connected clients
//! of one workspace, so that a notification (`selection_changed`, ...) can be sent to all of them.

use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CustomNotification,
    Implementation, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
    ServerNotification, Tool,
};
use rmcp::service::{
    NotificationContext, Peer, RequestContext, RxJsonRpcMessage, TxJsonRpcMessage,
};
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

struct ClientEntry {
    peer: Peer<RoleServer>,
    name: String,
    version: String,
    pid: Arc<Mutex<Option<u32>>>,
}

struct HubInner<B> {
    backend: B,
    server_name: String,
    server_version: String,
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
    /// A hub whose clients are told that the server is `server_name` / `server_version`.
    pub fn new(
        backend: B,
        server_name: impl Into<String>,
        server_version: impl Into<String>,
    ) -> Self {
        Self {
            inner: Arc::new(HubInner {
                backend,
                server_name: server_name.into(),
                server_version: server_version.into(),
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
            let handler = IdeHandler {
                hub: hub.clone(),
                pid: pid.clone(),
            };
            let Ok(running) = handler.serve(bridge(connection)).await else {
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
            hub.clients.lock().expect("not poisoned").insert(
                id,
                ClientEntry {
                    peer: running.peer().clone(),
                    name,
                    version,
                    pid,
                },
            );
            let _ = running.waiting().await;
            hub.clients.lock().expect("not poisoned").remove(&id);
        });
    }

    /// The clients that have completed their handshake and are still connected.
    pub fn clients(&self) -> Vec<ClientInfo> {
        let clients = self.inner.clients.lock().expect("not poisoned");
        let mut infos: Vec<(u64, ClientInfo)> = clients
            .iter()
            .map(|(id, entry)| {
                (
                    *id,
                    ClientInfo {
                        name: entry.name.clone(),
                        version: entry.version.clone(),
                        pid: *entry.pid.lock().expect("not poisoned"),
                    },
                )
            })
            .collect();
        infos.sort_by_key(|(id, _)| *id);
        infos.into_iter().map(|(_, info)| info).collect()
    }

    /// Sends a notification (a method of this protocol that MCP does not define, such as `selection_changed`) to every
    /// connected client. A client that cannot be reached is skipped.
    pub async fn notify(&self, method: &str, params: Value) {
        let peers: Vec<Peer<RoleServer>> = self
            .inner
            .clients
            .lock()
            .expect("not poisoned")
            .values()
            .map(|entry| entry.peer.clone())
            .collect();
        for peer in peers {
            let notification = CustomNotification::new(method, Some(params.clone()));
            let _ = peer
                .send_notification(ServerNotification::CustomNotification(notification))
                .await;
        }
    }
}

/// The handler of one client's connection.
struct IdeHandler<B> {
    hub: Arc<HubInner<B>>,
    pid: Arc<Mutex<Option<u32>>>,
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
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        match self.hub.backend.call(&request.name, arguments).await {
            Ok(ToolResult {
                text,
                is_error: false,
            }) => Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into()),
            Ok(ToolResult {
                text,
                is_error: true,
            }) => Ok(CallToolResult::error(vec![ContentBlock::text(text)]).into()),
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

fn bridge(mut connection: Connection) -> WsTransport {
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
