//! A workspace with an adapter registered and a Claude Code connected, as most tests of the MCP side start.

use claude_ide_core::jsonrpc::{Message, RpcError};
use claude_ide_core::workspace::Workspace;
use serde_json::{Value, json};
use tokio::io::DuplexStream;

use super::Client;
use super::claude::MockClaude;
use super::fixture::{Fixture, registered};

/// An adapter that declared `getOpenEditors` and `openFile` is registered, and Claude Code has connected to it and
/// completed the handshake.
pub struct Scene {
    pub fixture: Fixture,
    pub adapter: Client<DuplexStream>,
    pub workspace: Workspace,
    pub claude: MockClaude,
}

pub async fn scene_in(mut fixture: Fixture) -> Scene {
    let (adapter, workspace, port) = registered(&mut fixture).await;
    let token = fixture.read_lock(port).auth_token;
    let mut claude = MockClaude::connect(port, &token).await;
    claude.handshake(4321).await;
    claude.tool_names().await; // the handshake is done when this is answered
    Scene {
        fixture,
        adapter,
        workspace,
        claude,
    }
}

pub async fn scene() -> Scene {
    scene_in(Fixture::new()).await
}

/// The adapter answers the next request that it gets, and the request is returned.
pub async fn answer_next_request(
    adapter: &mut Client<DuplexStream>,
    outcome: Result<Value, RpcError>,
) -> (String, Value) {
    match adapter.recv().await {
        Some(Message::Request { id, method, params }) => {
            adapter.send(Message::Response { id, outcome }).await;
            (method, params)
        }
        other => panic!("expected a request, got {other:?}"),
    }
}

/// `tools/call` for a tool, while the adapter answers the one request it should get.
pub async fn call_tool(
    scene: &mut Scene,
    name: &str,
    arguments: Value,
    outcome: Result<Value, RpcError>,
) -> (Value, (String, Value)) {
    let call = scene.claude.request(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    );
    let (answer, request) = tokio::join!(call, answer_next_request(&mut scene.adapter, outcome));
    (answer, request)
}
