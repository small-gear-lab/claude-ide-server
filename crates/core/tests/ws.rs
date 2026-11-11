//! End-to-end tests of the WebSocket server, with a real WebSocket client on the loopback interface.

use std::time::Duration;

use claude_ide_core::ws::{AUTH_HEADER, Config, Error as ConnectionError, WsServer};
use futures_util::{SinkExt, StreamExt};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error, Message};

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(5);

async fn start(config: Config) -> WsServer {
    WsServer::start(TOKEN, config)
        .await
        .expect("the server starts")
}

type Client = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<TcpStream>>;

async fn connect(port: u16, token: Option<&str>) -> Result<Client, Error> {
    let mut request = format!("ws://127.0.0.1:{port}/")
        .into_client_request()
        .unwrap();
    if let Some(token) = token {
        request
            .headers_mut()
            .insert(AUTH_HEADER, token.parse().unwrap());
    }
    tokio_tungstenite::connect_async(request)
        .await
        .map(|(client, _response)| client)
}

#[tokio::test]
async fn a_client_with_the_right_token_connects_and_exchanges_messages() {
    let mut server = start(Config::default()).await;
    let mut client = connect(server.port(), Some(TOKEN))
        .await
        .expect("the upgrade succeeds");
    let mut accepted = timeout(WAIT, server.accept())
        .await
        .unwrap()
        .expect("a connection arrives");

    accepted.send("from the server").await.unwrap();
    assert_eq!(
        client.next().await.unwrap().unwrap(),
        Message::text("from the server")
    );
    client.send(Message::text("from the client")).await.unwrap();
    assert_eq!(accepted.recv().await.unwrap().unwrap(), "from the client");
}

#[tokio::test]
async fn a_wrong_or_missing_token_is_refused_during_the_upgrade() {
    let mut server = start(Config::default()).await;
    for token in [
        Some("wrong"),
        Some(""),
        Some("0123456789abcdef0123456789abcdef0"),
        None,
    ] {
        match connect(server.port(), token).await {
            Err(Error::Http(response)) => assert_eq!(response.status(), 401, "token {token:?}"),
            Err(other) => panic!("token {token:?}: unexpected error {other:?}"),
            Ok(_) => panic!("token {token:?} was accepted"),
        }
    }
    // Nothing reached the caller.
    assert!(
        timeout(Duration::from_millis(200), server.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn garbage_from_one_peer_does_not_affect_the_next() {
    let mut server = start(Config::default()).await;

    let mut raw = TcpStream::connect(server.local_addr()).await.unwrap();
    raw.write_all(b"\x00\xff not http at all \r\n\r\n")
        .await
        .unwrap();
    let mut buffer = [0u8; 64];
    // The server answers with an error or just closes; either way the read ends.
    let _ = timeout(WAIT, raw.read(&mut buffer))
        .await
        .expect("the server ends the bad connection");

    let _client = connect(server.port(), Some(TOKEN))
        .await
        .expect("a good client still connects");
    assert!(timeout(WAIT, server.accept()).await.unwrap().is_some());
}

#[tokio::test]
async fn a_silent_peer_does_not_hold_up_others_and_is_dropped_after_the_timeout() {
    let config = Config {
        handshake_timeout: Duration::from_millis(300),
        ..Config::default()
    };
    let mut server = start(config).await;

    let mut silent = TcpStream::connect(server.local_addr()).await.unwrap();

    // While the silent peer is still within its time, a good client goes through at once.
    let _client = connect(server.port(), Some(TOKEN))
        .await
        .expect("a good client connects");
    assert!(timeout(WAIT, server.accept()).await.unwrap().is_some());

    // The silent peer is cut off once the handshake timeout has passed.
    let mut buffer = [0u8; 16];
    let read = timeout(WAIT, silent.read(&mut buffer))
        .await
        .expect("the server closes it");
    assert!(matches!(read, Ok(0) | Err(_)));
}

#[tokio::test]
async fn an_oversized_message_ends_that_connection_only() {
    let config = Config {
        max_message_size: 1024,
        max_frame_size: 1024,
        ..Config::default()
    };
    let mut server = start(config).await;

    let mut client = connect(server.port(), Some(TOKEN)).await.unwrap();
    let mut accepted = timeout(WAIT, server.accept()).await.unwrap().unwrap();
    client.send(Message::binary(vec![0u8; 4096])).await.unwrap();
    let outcome = timeout(WAIT, accepted.recv()).await.unwrap();
    assert!(
        matches!(outcome, Some(Err(_)) | None),
        "the oversized message is not delivered: {outcome:?}"
    );

    let _next = connect(server.port(), Some(TOKEN))
        .await
        .expect("the server still accepts");
    assert!(timeout(WAIT, server.accept()).await.unwrap().is_some());
}

#[tokio::test]
async fn the_server_listens_on_the_loopback_interface_only() {
    let server = start(Config::default()).await;
    assert!(server.local_addr().ip().is_loopback());
    assert_ne!(server.port(), 0);
}

#[tokio::test]
async fn dropping_the_server_stops_it_listening() {
    let server = start(Config::default()).await;
    let addr = server.local_addr();
    drop(server);
    // Give the aborted task a moment to release the port.
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(TcpStream::connect(addr).await.is_err());
}

#[tokio::test]
async fn binary_frames_are_reported_and_a_close_ends_the_stream() {
    let mut server = start(Config::default()).await;
    let mut client = connect(server.port(), Some(TOKEN)).await.unwrap();
    let mut accepted = timeout(WAIT, server.accept()).await.unwrap().unwrap();

    client.send(Message::binary(vec![1, 2, 3])).await.unwrap();
    assert!(matches!(
        accepted.recv().await,
        Some(Err(ConnectionError::UnexpectedBinary))
    ));

    client.close(None).await.unwrap();
    assert!(timeout(WAIT, accepted.recv()).await.unwrap().is_none());
}

#[tokio::test]
async fn the_server_can_close_with_a_code_and_a_reason() {
    let mut server = start(Config::default()).await;
    let mut client = connect(server.port(), Some(TOKEN)).await.unwrap();
    let mut accepted = timeout(WAIT, server.accept()).await.unwrap().unwrap();

    accepted.close(1008, "policy violation").await;
    match timeout(WAIT, client.next()).await.unwrap() {
        Some(Ok(Message::Close(Some(frame)))) => {
            assert_eq!(u16::from(frame.code), 1008);
            assert_eq!(frame.reason.as_str(), "policy violation");
        }
        other => panic!("expected a close frame, got {other:?}"),
    }
}

#[tokio::test]
async fn dropping_the_server_closes_the_connections_it_accepted() {
    let mut server = start(Config::default()).await;
    let mut client = connect(server.port(), Some(TOKEN)).await.unwrap();
    let mut accepted = timeout(WAIT, server.accept()).await.unwrap().unwrap();

    drop(server);

    // The server's side ends, and the client is told that the server is going away.
    assert!(timeout(WAIT, accepted.recv()).await.unwrap().is_none());
    assert!(matches!(
        accepted.send("too late").await,
        Err(ConnectionError::Closed)
    ));
    match timeout(WAIT, client.next()).await.unwrap() {
        Some(Ok(Message::Close(Some(frame)))) => assert_eq!(u16::from(frame.code), 1001),
        other => panic!("expected a close frame, got {other:?}"),
    }
}
