//! The prototype of issue #5: the same probe as `mcp_probe`, but with the MCP protocol handled by `rmcp`, over a
//! transport that is built on `ws::Connection`. Not part of the server.
//!
//!     cargo run --example mcp_rmcp -- <workspace-folder> [log-file]

use std::future::Future;
use std::io::Write;
use std::sync::{Arc, Mutex};

use claude_ide_core::lock::{self, EnvVars, LockContents, LockDirs, LockFile};
use claude_ide_core::ws::{self, Connection, WsServer};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, CustomNotification,
    ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, ServerNotification,
    Tool,
};
use rmcp::service::{NotificationContext, RequestContext, RxJsonRpcMessage, TxJsonRpcMessage};
use rmcp::transport::Transport;
use rmcp::{ErrorData as McpError, RoleServer, ServerHandler, ServiceExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;

type Say = Arc<Mutex<Box<dyn FnMut(String) + Send>>>;

/// `rmcp` wants a transport whose `send` future is `'static` and can run concurrently, so the connection (which is
/// used through `&mut self`) is owned by a task, and the transport talks to that task through two channels.
struct WsTransport {
    outgoing: mpsc::Sender<String>,
    incoming: mpsc::Receiver<String>,
    say: Say,
}

fn pump(mut connection: Connection, say: Say) -> WsTransport {
    let (outgoing, mut to_send) = mpsc::channel::<String>(64);
    let (received, incoming) = mpsc::channel::<String>(64);
    tokio::spawn(async move {
        loop {
            tokio::select! {
                text = to_send.recv() => {
                    let Some(text) = text else { break };
                    if connection.send(text).await.is_err() { break }
                }
                message = connection.recv() => {
                    match message {
                        Some(Ok(text)) => if received.send(text).await.is_err() { break },
                        _ => break,
                    }
                }
            }
        }
    });
    WsTransport {
        outgoing,
        incoming,
        say,
    }
}

impl Transport<RoleServer> for WsTransport {
    type Error = std::io::Error;

    fn send(
        &mut self,
        item: TxJsonRpcMessage<RoleServer>,
    ) -> impl Future<Output = Result<(), Self::Error>> + Send + 'static {
        let outgoing = self.outgoing.clone();
        let say = self.say.clone();
        async move {
            let text = serde_json::to_string(&item).map_err(std::io::Error::other)?;
            (say.lock().unwrap())(format!("-> {text}"));
            outgoing
                .send(text)
                .await
                .map_err(|_| std::io::Error::from(std::io::ErrorKind::BrokenPipe))
        }
    }

    fn receive(&mut self) -> impl Future<Output = Option<RxJsonRpcMessage<RoleServer>>> + Send {
        async {
            loop {
                let text = self.incoming.recv().await?;
                (self.say.lock().unwrap())(format!("<- {text}"));
                match serde_json::from_str::<RxJsonRpcMessage<RoleServer>>(&text) {
                    Ok(message) => return Some(message),
                    Err(error) => (self.say.lock().unwrap())(format!("!! cannot parse: {error}")),
                }
            }
        }
    }

    fn close(&mut self) -> impl Future<Output = Result<(), Self::Error>> + Send {
        async { Ok(()) }
    }
}

#[derive(Clone)]
struct Handler {
    say: Say,
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let schema = json!({ "type": "object", "properties": {} });
        let schema = schema.as_object().unwrap().clone();
        Ok(ListToolsResult::with_all_items(vec![Tool::new(
            "getWorkspaceFolders",
            "Get all workspace folders currently open in the IDE",
            Arc::new(schema),
        )]))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        (self.say.lock().unwrap())(format!("tool call: {}", request.name));
        Ok(CallToolResult::success(vec![ContentBlock::text("{\"folders\":[]}")]).into())
    }

    async fn on_custom_notification(
        &self,
        notification: CustomNotification,
        _context: NotificationContext<RoleServer>,
    ) {
        (self.say.lock().unwrap())(format!(
            "custom notification: {} {:?}",
            notification.method, notification.params
        ));
    }
}

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let mut args = std::env::args().skip(1);
    let folder = args
        .next()
        .expect("usage: mcp_rmcp <workspace-folder> [log-file]");
    let log_path = args.next();
    let mut log = log_path.map(|path| std::fs::File::create(path).expect("the log file"));
    let say: Say = Arc::new(Mutex::new(Box::new(move |line: String| {
        eprintln!("{line}");
        if let Some(file) = log.as_mut() {
            let _ = writeln!(file, "{line}");
        }
    })));

    let dirs = LockDirs::from_env(&EnvVars::from_process())?;
    let token = lock::generate_token()?;
    let mut server = WsServer::start(token.clone(), ws::Config::default()).await?;
    let port = server.port();
    let _lock = LockFile::create(
        &dirs,
        port,
        LockContents {
            pid: std::process::id(),
            workspace_folders: vec![folder],
            ide_name: "claude-ide-server rmcp probe".to_owned(),
            transport: lock::TRANSPORT_WS.to_owned(),
            running_in_windows: false,
            auth_token: token,
        },
        std::process::id(),
    )?;
    (say.lock().unwrap())(format!("listening port={port}"));

    while let Some(connection) = server.accept().await {
        (say.lock().unwrap())("--- a client connected".to_owned());
        let transport = pump(connection, say.clone());
        let handler = Handler { say: say.clone() };
        match handler.serve(transport).await {
            Ok(running) => {
                let peer = running.peer().peer_info();
                (say.lock().unwrap())(format!(
                    "initialized; peer = {:?}",
                    peer.map(|p| p.client_info.clone())
                ));
                // A notification of this protocol that MCP does not define: sent as a custom one.
                let sent = running
                    .peer()
                    .send_notification(ServerNotification::CustomNotification(CustomNotification::new(
                        "selection_changed",
                        Some(json!({
                            "text": "",
                            "filePath": "/tmp/x",
                            "fileUrl": "file:///tmp/x",
                            "selection": { "start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 0}, "isEmpty": true },
                        })),
                    )))
                    .await;
                (say.lock().unwrap())(format!("sent selection_changed: {sent:?}"));
                let _ = running.waiting().await;
                (say.lock().unwrap())("--- the client went away".to_owned());
            }
            Err(error) => (say.lock().unwrap())(format!("!! serve failed: {error}")),
        }
    }
    let _ = Value::Null;
    Ok(())
}
