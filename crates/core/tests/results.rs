//! The result of a tool call may have several texts: the first is what the tool says, the others come after it (the answer
//! to `openDiff` is `FILE_SAVED` and then the contents of the file).

mod common;

use std::sync::Arc;
use std::time::Duration;

use claude_ide_core::mcp::{McpHub, ToolBackend, ToolDefinition, ToolError, ToolResult};
use claude_ide_core::ws::{self, WsServer};
use common::claude::MockClaude;
use serde_json::{Value, json};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(5);

/// A backend with one tool, which answers with two texts, and one that fails with two.
struct Texts;

impl ToolBackend for Texts {
    fn tools(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    async fn call(&self, name: &str, _arguments: Value) -> Result<ToolResult, ToolError> {
        match name {
            "two" => Ok(ToolResult {
                text: "first".to_owned(),
                more: vec!["second".to_owned()],
                is_error: false,
            }),
            "broken" => Ok(ToolResult {
                text: "what went wrong".to_owned(),
                more: vec!["and why".to_owned()],
                is_error: true,
            }),
            other => Err(ToolError::UnknownTool(other.to_owned())),
        }
    }
}

#[tokio::test]
async fn a_result_may_have_several_texts_in_order() {
    let mut server = WsServer::start(TOKEN, ws::Config::default()).await.unwrap();
    let port = server.port();
    let hub = McpHub::new(Texts, "test", "0", WAIT, Vec::new(), Arc::new(|_| {}));
    let accepting = tokio::spawn(async move {
        while let Some(connection) = server.accept().await {
            hub.attach(connection);
        }
    });
    let mut claude = MockClaude::connect(port, TOKEN).await;
    claude.handshake(4000).await;

    let answer = claude
        .request("tools/call", json!({"name": "two", "arguments": {}}))
        .await;
    let content = answer["result"]["content"].as_array().unwrap();
    assert_eq!(content.len(), 2, "{answer}");
    assert_eq!(content[0]["text"], "first");
    assert_eq!(content[1]["text"], "second");
    assert_eq!(content[1]["type"], "text");
    assert_eq!(answer["result"]["isError"], false);

    let answer = claude
        .request("tools/call", json!({"name": "broken", "arguments": {}}))
        .await;
    assert_eq!(answer["result"]["isError"], true);
    assert_eq!(answer["result"]["content"][1]["text"], "and why");
    accepting.abort();
}
