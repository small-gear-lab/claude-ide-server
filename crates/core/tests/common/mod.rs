//! Helpers shared by the integration tests of the adapter side and of the workspaces.

// Each test file uses some of these, not all.
#![allow(dead_code)]

pub mod claude;
pub mod fixture;
pub mod scene;
pub mod shape;

use std::time::Duration;

use claude_ide_core::adapter::PROTOCOL_VERSION;
use claude_ide_core::jsonrpc::{self, Message, RpcError};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, BufReader, ReadHalf, WriteHalf};
use tokio::time::timeout;

pub const WAIT: Duration = Duration::from_secs(5);

/// What an adapter connects with on this system: a Unix socket, or a named pipe on Windows.
#[cfg(unix)]
pub type AdapterStream = tokio::net::UnixStream;
#[cfg(windows)]
pub type AdapterStream = tokio::net::windows::named_pipe::NamedPipeClient;

/// Connects to the adapter socket at `path` (on Windows, the name of the pipe), as an adapter does.
pub async fn connect_to_adapters(path: &std::path::Path) -> std::io::Result<AdapterStream> {
    #[cfg(unix)]
    {
        tokio::net::UnixStream::connect(path).await
    }
    #[cfg(windows)]
    {
        // A pipe whose instances are all in use for a moment (the next one is being made) is tried again.
        const ERROR_PIPE_BUSY: i32 = 231;
        timeout(WAIT, async {
            loop {
                match tokio::net::windows::named_pipe::ClientOptions::new().open(path) {
                    Err(error) if error.raw_os_error() == Some(ERROR_PIPE_BUSY) => {
                        tokio::time::sleep(Duration::from_millis(10)).await;
                    }
                    other => return other,
                }
            }
        })
        .await
        .unwrap_or_else(|_| Err(std::io::Error::from(std::io::ErrorKind::TimedOut)))
    }
}

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
