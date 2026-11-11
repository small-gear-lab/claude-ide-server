//! JSON-RPC 2.0 messages, and their framing with a `Content-Length` header (as in LSP).
//!
//! This is the language that adapters speak to the server. It works over any byte stream (`AsyncBufRead` to read,
//! `AsyncWrite` to write).

use std::fmt;
use std::io;

use serde_json::{Value, json};
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};

/// The largest header section (the lines before the empty line) that is accepted.
const MAX_HEADER_BYTES: usize = 8 * 1024;

/// The error of a response.
#[derive(Debug, Clone, PartialEq)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JSON-RPC error {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// The error codes that JSON-RPC itself defines. A protocol on top of it adds its own (see `adapter::error_code`).
pub mod code {
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
}

/// One JSON-RPC message.
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request {
        id: Value,
        method: String,
        params: Value,
    },
    Notification {
        method: String,
        params: Value,
    },
    Response {
        id: Value,
        outcome: Result<Value, RpcError>,
    },
}

impl Message {
    /// The message as JSON.
    pub fn to_value(&self) -> Value {
        match self {
            Self::Request { id, method, params } => {
                json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
            }
            Self::Notification { method, params } => {
                json!({ "jsonrpc": "2.0", "method": method, "params": params })
            }
            Self::Response {
                id,
                outcome: Ok(result),
            } => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Self::Response {
                id,
                outcome: Err(error),
            } => {
                let mut body = json!({ "code": error.code, "message": error.message });
                if let Some(data) = &error.data {
                    body["data"] = data.clone();
                }
                json!({ "jsonrpc": "2.0", "id": id, "error": body })
            }
        }
    }

    /// Reads a message from JSON.
    pub fn from_value(value: &Value) -> Result<Self, FrameError> {
        let malformed = |what: &str| FrameError::Malformed(what.to_owned());
        let object = value
            .as_object()
            .ok_or_else(|| malformed("a message must be a JSON object"))?;
        if object.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(malformed("\"jsonrpc\" must be \"2.0\""));
        }
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        match (object.get("method"), object.get("id")) {
            (Some(method), id) => {
                let method = method
                    .as_str()
                    .ok_or_else(|| malformed("\"method\" must be a string"))?
                    .to_owned();
                Ok(match id {
                    Some(id) => Self::Request {
                        id: id.clone(),
                        method,
                        params,
                    },
                    None => Self::Notification { method, params },
                })
            }
            (None, Some(id)) => {
                let outcome = match (object.get("result"), object.get("error")) {
                    (Some(result), None) => Ok(result.clone()),
                    (None, Some(error)) => {
                        let code = error
                            .get("code")
                            .and_then(Value::as_i64)
                            .ok_or_else(|| malformed("an error needs an integer \"code\""))?;
                        let message = error
                            .get("message")
                            .and_then(Value::as_str)
                            .ok_or_else(|| malformed("an error needs a string \"message\""))?;
                        Err(RpcError {
                            code,
                            message: message.to_owned(),
                            data: error.get("data").cloned(),
                        })
                    }
                    _ => {
                        return Err(malformed(
                            "a response has \"result\" or \"error\", not both",
                        ));
                    }
                };
                Ok(Self::Response {
                    id: id.clone(),
                    outcome,
                })
            }
            (None, None) => Err(malformed("neither \"method\" nor \"id\"")),
        }
    }
}

/// What can go wrong while reading a message.
#[derive(Debug)]
pub enum FrameError {
    /// The stream failed, or ended in the middle of a message.
    Io(io::Error),
    /// The header or the JSON is not what the protocol allows.
    Malformed(String),
    /// The body is larger than the limit.
    TooLarge(usize),
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "I/O error: {error}"),
            Self::Malformed(what) => write!(f, "malformed message: {what}"),
            Self::TooLarge(size) => write!(f, "message of {size} bytes is over the limit"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for FrameError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Reads one message. `Ok(None)` means the stream ended cleanly, between messages.
///
/// Not cancellation safe: do not use it as a branch of `select!`; read in a task of its own.
pub async fn read_message<R>(
    reader: &mut R,
    max_body_bytes: usize,
) -> Result<Option<Message>, FrameError>
where
    R: AsyncBufRead + AsyncRead + Unpin,
{
    let mut content_length: Option<usize> = None;
    let mut header_bytes = 0usize;
    let mut first_line = true;
    loop {
        let mut line = String::new();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            return if first_line {
                Ok(None)
            } else {
                Err(FrameError::Io(io::ErrorKind::UnexpectedEof.into()))
            };
        }
        first_line = false;
        header_bytes += read;
        if header_bytes > MAX_HEADER_BYTES {
            return Err(FrameError::Malformed("the header is too long".to_owned()));
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        let Some((name, value)) = line.split_once(':') else {
            return Err(FrameError::Malformed(format!(
                "a header line without a colon: {line:?}"
            )));
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length =
                Some(value.trim().parse().map_err(|_| {
                    FrameError::Malformed("Content-Length is not a number".to_owned())
                })?);
        }
    }
    let length = content_length
        .ok_or_else(|| FrameError::Malformed("the header has no Content-Length".to_owned()))?;
    if length > max_body_bytes {
        return Err(FrameError::TooLarge(length));
    }
    let mut body = vec![0u8; length];
    reader.read_exact(&mut body).await?;
    let value: Value = serde_json::from_slice(&body)
        .map_err(|error| FrameError::Malformed(format!("the body is not JSON: {error}")))?;
    Message::from_value(&value).map(Some)
}

/// Writes one message with its header.
pub async fn write_message<W>(writer: &mut W, message: &Message) -> io::Result<()>
where
    W: AsyncWrite + Unpin,
{
    let body = serde_json::to_vec(&message.to_value()).map_err(io::Error::other)?;
    let mut frame = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    frame.extend_from_slice(&body);
    writer.write_all(&frame).await?;
    writer.flush().await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufReader;

    async fn read(bytes: &[u8]) -> Result<Option<Message>, FrameError> {
        read_message(&mut BufReader::new(bytes), 1024).await
    }

    fn frame(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{body}", body.len()).into_bytes()
    }

    #[tokio::test]
    async fn a_request_a_notification_and_a_response_round_trip() {
        let messages = [
            Message::Request {
                id: json!(7),
                method: "register".into(),
                params: json!({"a": [1, 2]}),
            },
            Message::Notification {
                method: "shutdown".into(),
                params: json!({}),
            },
            Message::Response {
                id: json!("x"),
                outcome: Ok(json!({"port": 1})),
            },
            Message::Response {
                id: json!(8),
                outcome: Err(RpcError {
                    code: -32000,
                    message: "no".into(),
                    data: Some(json!([1])),
                }),
            },
        ];
        let mut bytes = Vec::new();
        for message in &messages {
            write_message(&mut bytes, message).await.unwrap();
        }
        let mut reader = BufReader::new(&bytes[..]);
        for message in &messages {
            assert_eq!(
                read_message(&mut reader, 1024).await.unwrap().as_ref(),
                Some(message)
            );
        }
        assert!(read_message(&mut reader, 1024).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_length_counts_bytes_not_characters() {
        let body = r#"{"jsonrpc":"2.0","method":"m","params":{"text":"日本語"}}"#;
        let message = read(&frame(body)).await.unwrap().unwrap();
        assert_eq!(
            message,
            Message::Notification {
                method: "m".into(),
                params: json!({"text": "日本語"})
            }
        );
    }

    #[tokio::test]
    async fn headers_may_be_in_any_case_and_come_with_others() {
        let body = r#"{"jsonrpc":"2.0","method":"m"}"#;
        let bytes = format!(
            "Content-Type: application/vscode-jsonrpc; charset=utf-8\r\ncontent-length: {}\r\n\r\n{body}",
            body.len()
        );
        assert!(read(bytes.as_bytes()).await.unwrap().is_some());
    }

    #[tokio::test]
    async fn bad_input_is_reported_not_swallowed() {
        // No Content-Length.
        assert!(matches!(
            read(b"X: 1\r\n\r\n{}").await,
            Err(FrameError::Malformed(_))
        ));
        // Not a number.
        assert!(matches!(
            read(b"Content-Length: many\r\n\r\n").await,
            Err(FrameError::Malformed(_))
        ));
        // A line without a colon.
        assert!(matches!(
            read(b"hello\r\n\r\n").await,
            Err(FrameError::Malformed(_))
        ));
        // Over the limit.
        assert!(matches!(
            read(b"Content-Length: 99999\r\n\r\n").await,
            Err(FrameError::TooLarge(99999))
        ));
        // The body is not JSON; a JSON value that is not a message.
        assert!(matches!(
            read(&frame("not json")).await,
            Err(FrameError::Malformed(_))
        ));
        assert!(matches!(
            read(&frame("[1]")).await,
            Err(FrameError::Malformed(_))
        ));
        assert!(matches!(
            read(&frame(r#"{"jsonrpc":"1.0","method":"m"}"#)).await,
            Err(FrameError::Malformed(_))
        ));
        // A header that never ends.
        let endless = format!("X: {}\r\n", "a".repeat(9000));
        assert!(matches!(
            read(endless.as_bytes()).await,
            Err(FrameError::Malformed(_))
        ));
    }

    #[tokio::test]
    async fn an_end_in_the_middle_of_a_message_is_an_error_but_between_messages_is_not() {
        assert!(read(b"").await.unwrap().is_none());
        assert!(matches!(
            read(b"Content-Length: 10\r\n").await,
            Err(FrameError::Io(_))
        ));
        assert!(matches!(
            read(b"Content-Length: 10\r\n\r\n{}").await,
            Err(FrameError::Io(_))
        ));
    }

    #[tokio::test]
    async fn a_message_that_arrives_in_pieces_is_read_whole() {
        let body = r#"{"jsonrpc":"2.0","method":"m"}"#;
        let bytes = frame(body);
        let (mut writer, reader) = tokio::io::duplex(8);
        let write = tokio::spawn(async move {
            for chunk in bytes.chunks(3) {
                writer.write_all(chunk).await.unwrap();
                tokio::task::yield_now().await;
            }
        });
        let message = read_message(&mut BufReader::new(reader), 1024)
            .await
            .unwrap();
        write.await.unwrap();
        assert!(matches!(message, Some(Message::Notification { .. })));
    }
}
