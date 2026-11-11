//! A workspace with an adapter registered and a Claude Code connected, as most tests of the MCP side start.

use claude_ide_core::jsonrpc::{Message, RpcError};
use claude_ide_core::workspace::Workspace;
use serde_json::{Value, json};
use tokio::io::DuplexStream;

use super::Client;
use super::claude::MockClaude;
use super::fixture::{Fixture, registered_with};

/// An adapter that declared `getOpenEditors` and `openFile` is registered, and Claude Code has connected to it and
/// completed the handshake.
pub struct Scene {
    pub fixture: Fixture,
    pub adapter: Client<DuplexStream>,
    pub workspace: Workspace,
    pub claude: MockClaude,
}

pub async fn scene_in(fixture: Fixture) -> Scene {
    scene_with(fixture, super::registration()).await
}

/// Like [`scene_in`], for an adapter that registers with `params`.
pub async fn scene_with(mut fixture: Fixture, params: Value) -> Scene {
    let (adapter, workspace, port) = registered_with(&mut fixture, params).await;
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

/// An adapter that declares these methods, and nothing else different from [`registration`](super::registration).
pub async fn scene_declaring(methods: &[&str]) -> Scene {
    let mut params = super::registration();
    params["methods"] = json!(methods);
    scene_with(Fixture::new(), params).await
}

/// The JSON that a tool result carries as its text.
pub fn result_json(answer: &Value) -> Value {
    let text = answer["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_else(|| panic!("no text in {answer}"));
    serde_json::from_str(text).unwrap_or_else(|e| panic!("{text}: {e}"))
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
