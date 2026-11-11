//! A stand-in for Claude Code's MCP client: a WebSocket client that says what the real CLI (2.1.322) was seen to say.

use std::time::Duration;

use claude_ide_core::ws::AUTH_HEADER;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

const WAIT: Duration = Duration::from_secs(5);

pub struct MockClaude {
    stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    next_id: u64,
}

impl MockClaude {
    /// Connects the way the CLI does: with the token, and offering the subprotocol `mcp`, which must be picked.
    pub async fn connect(port: u16, token: &str) -> Self {
        let mut request = format!("ws://127.0.0.1:{port}/")
            .into_client_request()
            .unwrap();
        request
            .headers_mut()
            .insert(AUTH_HEADER, token.parse().unwrap());
        request
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "mcp".parse().unwrap());
        let (stream, response) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(
            response.headers().get("sec-websocket-protocol").unwrap(),
            "mcp"
        );
        Self { stream, next_id: 0 }
    }

    /// Sends a text frame as it is.
    pub async fn send_raw(&mut self, text: &str) {
        self.stream.send(Message::text(text)).await.unwrap();
    }

    pub async fn send_json(&mut self, value: Value) {
        self.stream
            .send(Message::text(value.to_string()))
            .await
            .unwrap();
    }

    /// The next message from the server, or `None` when it has closed the connection.
    pub async fn recv_json(&mut self) -> Option<Value> {
        loop {
            match timeout(WAIT, self.stream.next())
                .await
                .expect("a message or the end of the connection")?
            {
                Ok(Message::Text(text)) => {
                    return Some(serde_json::from_str(text.as_str()).unwrap());
                }
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    /// The close code of the server's closing frame, if it closes the connection.
    pub async fn close_code(&mut self) -> Option<u16> {
        loop {
            match timeout(WAIT, self.stream.next())
                .await
                .expect("a close frame or the end of the connection")?
            {
                Ok(Message::Close(Some(frame))) => return Some(u16::from(frame.code)),
                Ok(Message::Close(None)) | Err(_) => return None,
                Ok(_) => {}
            }
        }
    }

    /// Sends a request and returns its response (the whole message: `result` or `error`). Notifications that arrive
    /// in the meantime are collected in `notifications`.
    pub async fn request_collecting(
        &mut self,
        method: &str,
        params: Value,
        notifications: &mut Vec<Value>,
    ) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send_json(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
            .await;
        loop {
            let message = self.recv_json().await.expect("an answer");
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                return message;
            }
            notifications.push(message);
        }
    }

    pub async fn request(&mut self, method: &str, params: Value) -> Value {
        self.request_collecting(method, params, &mut Vec::new())
            .await
    }

    pub async fn notify(&mut self, method: &str, params: Value) {
        self.send_json(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
            .await;
    }

    /// What the CLI sends first: `initialize`, `notifications/initialized`, and `ide_connected`. Returns the answer to
    /// `initialize`.
    pub async fn handshake(&mut self, pid: u32) -> Value {
        let answer = self
            .request(
                "initialize",
                super::shape::fixture("claude_initialize.json"),
            )
            .await;
        self.notify("notifications/initialized", json!({})).await;
        self.notify("ide_connected", json!({ "pid": pid })).await;
        answer
    }

    /// The names of the tools in the answer to `tools/list`.
    pub async fn tool_names(&mut self) -> Vec<String> {
        let answer = self.request("tools/list", json!({})).await;
        answer["result"]["tools"]
            .as_array()
            .unwrap_or_else(|| panic!("no tools in {answer}"))
            .iter()
            .map(|tool| tool["name"].as_str().unwrap().to_owned())
            .collect()
    }
}
