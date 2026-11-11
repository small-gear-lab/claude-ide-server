//! Helpers shared by the integration tests of the adapter side and of the workspaces.

// Each test file uses some of these, not all.
#![allow(dead_code)]

use std::time::Duration;

use claude_ide_core::adapter::PROTOCOL_VERSION;
use claude_ide_core::jsonrpc::{self, Message, RpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, BufReader, ReadHalf, WriteHalf};
use tokio::time::timeout;

pub const WAIT: Duration = Duration::from_secs(5);

/// The adapter's end of a connection: speaks the framing, one message at a time.
pub struct Client<S> {
    reader: BufReader<ReadHalf<S>>,
    writer: WriteHalf<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Client<S> {
    pub fn new(stream: S) -> Self {
        let (read, writer) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(read),
            writer,
        }
    }

    pub async fn send(&mut self, message: Message) {
        jsonrpc::write_message(&mut self.writer, &message)
            .await
            .unwrap();
    }

    pub async fn request(&mut self, id: u64, method: &str, params: Value) {
        self.send(Message::Request {
            id: json!(id),
            method: method.to_owned(),
            params,
        })
        .await;
    }

    pub async fn notify(&mut self, method: &str, params: Value) {
        self.send(Message::Notification {
            method: method.to_owned(),
            params,
        })
        .await;
    }

    /// The next message, or `None` when the server has closed the stream.
    pub async fn recv(&mut self) -> Option<Message> {
        timeout(WAIT, jsonrpc::read_message(&mut self.reader, 1 << 20))
            .await
            .expect("a message or the end of the stream arrives")
            .ok()
            .flatten()
    }

    /// Sends `register` and returns the port, or the error that answered it.
    pub async fn register(&mut self, params: Value) -> Result<u16, RpcError> {
        self.request(1, "register", params).await;
        self.registered().await
    }

    /// Reads the answer to a `register` that was already sent.
    pub async fn registered(&mut self) -> Result<u16, RpcError> {
        match self.recv().await.expect("an answer") {
            Message::Response {
                outcome: Ok(result),
                ..
            } => Ok(result["port"].as_u64().unwrap() as u16),
            Message::Response {
                outcome: Err(error),
                ..
            } => Err(error),
            other => panic!("not an answer: {other:?}"),
        }
    }
}

/// A `register` that the server accepts.
pub fn registration() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "ideName": "Neovim",
        "pid": 1234,
        "workspaceFolders": ["/home/user/project"],
        "methods": ["getOpenEditors", "openFile"],
    })
}
