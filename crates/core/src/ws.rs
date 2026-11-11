//! The WebSocket server that Claude Code connects to.
//!
//! It listens on the loopback interface only. A connection is handed out only after the HTTP upgrade has carried
//! the right token in the `x-claude-code-ide-authorization` header; anything else is refused during the upgrade
//! and never reaches the caller. A bad or slow peer affects only its own connection.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
use tokio_tungstenite::tungstenite::http::StatusCode;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

/// The header that carries the token. Claude Code sends it with the value from the lock file.
pub const AUTH_HEADER: &str = "x-claude-code-ide-authorization";

/// An accepted, authenticated connection.
pub type Connection = WebSocketStream<TcpStream>;

/// Limits and timeouts of the server.
#[derive(Debug, Clone)]
pub struct Config {
    /// How long a peer may take to finish the HTTP upgrade.
    pub handshake_timeout: Duration,
    /// The largest message (after reassembling fragments) that a peer may send.
    pub max_message_size: usize,
    /// The largest single frame that a peer may send.
    pub max_frame_size: usize,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            handshake_timeout: Duration::from_secs(5),
            max_message_size: 16 << 20,
            max_frame_size: 4 << 20,
        }
    }
}

/// A listening WebSocket server. Dropping it stops listening.
///
/// Connections that have passed the upgrade and the token check arrive on the receiver returned by [`start`].
///
/// [`start`]: WsServer::start
#[derive(Debug)]
pub struct WsServer {
    addr: SocketAddr,
    task: JoinHandle<()>,
}

impl WsServer {
    /// Starts listening on `127.0.0.1`, on a free port that the operating system picks (the ephemeral range, which
    /// lies inside 10000-65535 on Linux, macOS and Windows).
    pub async fn start(
        token: impl Into<String>,
        config: Config,
    ) -> io::Result<(Self, mpsc::Receiver<Connection>)> {
        let listener =
            TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0)).await?;
        let addr = listener.local_addr()?;
        let (tx, rx) = mpsc::channel(16);
        let token: std::sync::Arc<str> = token.into().into();
        let task = tokio::spawn(accept_loop(listener, token, config, tx));
        Ok((Self { addr, task }, rx))
    }

    /// The port that the server listens on.
    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// The address that the server listens on.
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }
}

impl Drop for WsServer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn accept_loop(
    listener: TcpListener,
    token: std::sync::Arc<str>,
    config: Config,
    tx: mpsc::Sender<Connection>,
) {
    loop {
        let Ok((stream, _peer)) = listener.accept().await else {
            // A failed accept (for example, too many open files) must not end the server.
            tokio::time::sleep(Duration::from_millis(50)).await;
            continue;
        };
        if tx.is_closed() {
            return;
        }
        let token = token.clone();
        let config = config.clone();
        let tx = tx.clone();
        // Each upgrade runs in its own task, so a slow or hostile peer cannot hold up the others.
        tokio::spawn(async move {
            let upgraded =
                tokio::time::timeout(config.handshake_timeout, upgrade(stream, &token, &config))
                    .await;
            if let Ok(Ok(connection)) = upgraded {
                let _ = tx.send(connection).await;
            }
        });
    }
}

async fn upgrade(
    stream: TcpStream,
    token: &str,
    config: &Config,
) -> Result<Connection, tokio_tungstenite::tungstenite::Error> {
    // The callback's signature, with its large error type, is fixed by tungstenite.
    #[allow(clippy::result_large_err)]
    let check = |request: &Request, response: Response| -> Result<Response, ErrorResponse> {
        let given = request
            .headers()
            .get(AUTH_HEADER)
            .map(|value| value.as_bytes());
        match given {
            Some(given) if constant_time_eq(given, token.as_bytes()) => Ok(response),
            _ => {
                let mut refusal = ErrorResponse::new(Some("Unauthorized".to_owned()));
                *refusal.status_mut() = StatusCode::UNAUTHORIZED;
                Err(refusal)
            }
        }
    };
    let websocket_config = WebSocketConfig::default()
        .max_message_size(Some(config.max_message_size))
        .max_frame_size(Some(config.max_frame_size));
    tokio_tungstenite::accept_hdr_async_with_config(stream, check, Some(websocket_config)).await
}

/// Compares two byte strings without stopping at the first difference.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b)
        .fold(0u8, |difference, (x, y)| difference | (x ^ y))
        == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_matches_equality() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
}
