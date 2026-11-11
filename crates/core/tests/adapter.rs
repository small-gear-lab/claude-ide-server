//! Tests of the adapter side alone: the framing, `register`, the requests. There are no ports and no lock files
//! here; a test plays the part of the layer above, and answers each registration itself.

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use claude_ide_core::adapter::memory::{MemoryConnector, listener};
use claude_ide_core::adapter::{
    AdapterServer, AdapterSession, Config, Notification, Registration, RequestError, UnixListener,
    error_code,
};
use claude_ide_core::jsonrpc::{Message, RpcError, code};
use common::{Client, WAIT, registration};
use serde_json::json;
use tempfile::TempDir;
use tokio::io::DuplexStream;
use tokio::time::timeout;

const PORT: u16 = 4000;

struct Fixture {
    connector: MemoryConnector,
    server: AdapterServer,
}

impl Fixture {
    fn new() -> Self {
        Self::with(|_| {})
    }

    fn with(tweak: impl FnOnce(&mut Config)) -> Self {
        let mut config = Config::default();
        tweak(&mut config);
        let (listener, connector) = listener();
        Self {
            connector,
            server: AdapterServer::start(listener, config),
        }
    }

    async fn client(&self) -> Client<DuplexStream> {
        Client::new(self.connector.connect().await.unwrap())
    }

    /// Whether a registration is handed over within a short time.
    async fn nothing_arrives(&mut self) -> bool {
        timeout(Duration::from_millis(200), self.server.accept())
            .await
            .is_err()
    }
}

/// An adapter registers and is accepted with `PORT`.
async fn registered(fixture: &mut Fixture) -> (Client<DuplexStream>, Registration, AdapterSession) {
    let mut client = fixture.client().await;
    client.request(1, "register", registration()).await;
    let registering = timeout(WAIT, fixture.server.accept())
        .await
        .unwrap()
        .expect("a registration");
    let info = registering.registration().clone();
    let session = registering.accept(PORT);
    assert_eq!(client.registered().await.unwrap(), PORT);
    (client, info, session)
}

#[tokio::test]
async fn a_registration_is_handed_over_and_the_adapter_is_told_the_port() {
    let mut fixture = Fixture::new();
    let (_client, info, _session) = registered(&mut fixture).await;
    assert_eq!(info.ide_name, "Neovim");
    assert_eq!(info.pid, 1234);
    assert_eq!(info.workspace_folders, ["/home/user/project"]);
    assert_eq!(info.methods, ["getOpenEditors", "openFile"]);
}

#[tokio::test]
async fn a_registration_that_is_not_acceptable_gets_an_error_and_is_not_handed_over() {
    let mut fixture = Fixture::new();
    let cases = [
        (
            json!({"protocolVersion": 99, "ideName": "x", "pid": 1, "workspaceFolders": ["/a"], "methods": []}),
            error_code::UNSUPPORTED_PROTOCOL_VERSION,
        ),
        (
            json!({"protocolVersion": 1, "pid": 1, "workspaceFolders": ["/a"], "methods": []}),
            code::INVALID_PARAMS,
        ),
        (
            json!({"protocolVersion": 1, "ideName": "x", "pid": 0, "workspaceFolders": ["/a"], "methods": []}),
            code::INVALID_PARAMS,
        ),
        (
            json!({"protocolVersion": 1, "ideName": "x", "pid": 1, "workspaceFolders": [], "methods": []}),
            code::INVALID_PARAMS,
        ),
        (
            json!({"protocolVersion": 1, "ideName": "x", "pid": 1, "workspaceFolders": [1], "methods": []}),
            code::INVALID_PARAMS,
        ),
        (
            json!({"protocolVersion": 1, "ideName": "x", "pid": 1, "workspaceFolders": ["/a"]}),
            code::INVALID_PARAMS,
        ),
    ];
    for (params, expected) in cases {
        let mut client = fixture.client().await;
        let error = client.register(params.clone()).await.unwrap_err();
        assert_eq!(error.code, expected, "{params}");
        assert!(client.recv().await.is_none(), "the connection is closed");
    }
    assert!(fixture.nothing_arrives().await);
}

#[tokio::test]
async fn the_first_message_must_be_register() {
    let mut fixture = Fixture::new();

    let mut client = fixture.client().await;
    client.request(5, "selectionChanged", json!({})).await;
    match client.recv().await {
        Some(Message::Response {
            id,
            outcome: Err(error),
        }) => {
            assert_eq!(id, json!(5));
            assert_eq!(error.code, error_code::NOT_REGISTERED);
        }
        other => panic!("expected an error answer, got {other:?}"),
    }
    assert!(client.recv().await.is_none());

    // A notification cannot be answered: the connection is simply closed.
    let mut client = fixture.client().await;
    client.notify("selectionChanged", json!({})).await;
    assert!(client.recv().await.is_none());
    assert!(fixture.nothing_arrives().await);
}

#[tokio::test]
async fn a_silent_connection_is_closed_after_the_register_timeout() {
    let mut fixture = Fixture::with(|config| config.register_timeout = Duration::from_millis(200));
    let mut client = fixture.client().await;
    assert!(client.recv().await.is_none());
    assert!(fixture.nothing_arrives().await);
}

#[tokio::test]
async fn a_rejection_reaches_the_adapter_as_an_error_and_ends_the_connection() {
    let mut fixture = Fixture::new();
    let mut client = fixture.client().await;
    client.request(1, "register", registration()).await;
    let registering = timeout(WAIT, fixture.server.accept())
        .await
        .unwrap()
        .unwrap();
    registering.reject("no port today");

    let error = client.registered().await.unwrap_err();
    assert_eq!(error.code, error_code::SETUP_FAILED);
    assert_eq!(error.message, "no port today");
    assert!(client.recv().await.is_none());
}

#[tokio::test]
async fn a_registration_that_is_dropped_without_an_answer_ends_the_connection() {
    let mut fixture = Fixture::new();
    let mut client = fixture.client().await;
    client.request(1, "register", registration()).await;
    drop(
        timeout(WAIT, fixture.server.accept())
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(client.recv().await.is_none());
}

#[tokio::test]
async fn an_adapter_that_leaves_before_the_answer_does_no_harm() {
    let mut fixture = Fixture::new();
    let mut client = fixture.client().await;
    client.request(1, "register", registration()).await;
    let registering = timeout(WAIT, fixture.server.accept())
        .await
        .unwrap()
        .unwrap();
    drop(client);
    tokio::time::sleep(Duration::from_millis(100)).await;

    let mut session = registering.accept(PORT);
    assert!(
        timeout(WAIT, session.next_notification())
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn notifications_arrive_in_order() {
    let mut fixture = Fixture::new();
    let (mut client, _info, mut session) = registered(&mut fixture).await;
    client
        .notify("selectionChanged", json!({"filePath": "/a"}))
        .await;
    client
        .notify("diagnosticsChanged", json!({"uris": []}))
        .await;
    for (method, params) in [
        ("selectionChanged", json!({"filePath": "/a"})),
        ("diagnosticsChanged", json!({"uris": []})),
    ] {
        assert_eq!(
            timeout(WAIT, session.next_notification())
                .await
                .unwrap()
                .unwrap(),
            Notification {
                method: method.to_owned(),
                params
            }
        );
    }
}

#[tokio::test]
async fn when_the_adapter_goes_away_the_session_ends_and_waiting_requests_fail() {
    let mut fixture = Fixture::new();
    let (client, _info, mut session) = registered(&mut fixture).await;
    let handle = session.handle();

    // A request is waiting for an answer that will never come.
    let waiting = tokio::spawn({
        let handle = handle.clone();
        async move { handle.request("getOpenEditors", json!({})).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(client);

    assert!(
        timeout(WAIT, session.next_notification())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        timeout(WAIT, waiting).await.unwrap().unwrap(),
        Err(RequestError::Disconnected)
    );
    assert_eq!(
        handle.request("getOpenEditors", json!({})).await,
        Err(RequestError::Disconnected)
    );
}

#[tokio::test]
async fn requests_to_the_adapter_are_answered_matched_and_time_out() {
    let mut fixture = Fixture::with(|config| config.request_timeout = Duration::from_millis(300));
    let (mut client, _info, session) = registered(&mut fixture).await;
    let handle = session.handle();

    // An answer with a result.
    let first = tokio::spawn({
        let handle = handle.clone();
        async move { handle.request("getOpenEditors", json!({"x": 1})).await }
    });
    let id = match client.recv().await {
        Some(Message::Request { id, method, params }) => {
            assert_eq!(method, "getOpenEditors");
            assert_eq!(params, json!({"x": 1}));
            id
        }
        other => panic!("expected a request, got {other:?}"),
    };
    client
        .send(Message::Response {
            id,
            outcome: Ok(json!({"tabs": []})),
        })
        .await;
    assert_eq!(first.await.unwrap(), Ok(json!({"tabs": []})));

    // An answer with an error.
    let second = tokio::spawn({
        let handle = handle.clone();
        async move { handle.request("openFile", json!({})).await }
    });
    let id = match client.recv().await {
        Some(Message::Request { id, .. }) => id,
        other => panic!("expected a request, got {other:?}"),
    };
    client
        .send(Message::Response {
            id,
            outcome: Err(RpcError::new(-1, "no such file")),
        })
        .await;
    assert_eq!(
        second.await.unwrap(),
        Err(RequestError::Rpc(RpcError::new(-1, "no such file")))
    );

    // A method that was not declared in register is not called at all.
    assert_eq!(
        handle.request("saveDocument", json!({})).await,
        Err(RequestError::NotDeclared("saveDocument".to_owned()))
    );

    // No answer at all.
    assert_eq!(
        handle.request("openFile", json!({})).await,
        Err(RequestError::Timeout)
    );

    // A notification goes out as one.
    handle
        .notify("shutdownSoon", json!({"in": 1}))
        .await
        .unwrap();
    loop {
        match client.recv().await {
            Some(Message::Notification { method, params }) => {
                assert_eq!(method, "shutdownSoon");
                assert_eq!(params, json!({"in": 1}));
                break;
            }
            // The request that timed out is still in the stream.
            Some(Message::Request { .. }) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn closing_or_dropping_the_session_says_shutdown() {
    let mut fixture = Fixture::new();

    let (mut client, _info, session) = registered(&mut fixture).await;
    timeout(WAIT, session.close()).await.unwrap();
    match client.recv().await {
        Some(Message::Notification { method, .. }) => assert_eq!(method, "shutdown"),
        other => panic!("expected shutdown, got {other:?}"),
    }
    assert!(client.recv().await.is_none());

    let (mut client, _info, session) = registered(&mut fixture).await;
    drop(session);
    match client.recv().await {
        Some(Message::Notification { method, .. }) => assert_eq!(method, "shutdown"),
        other => panic!("expected shutdown, got {other:?}"),
    }
    assert!(client.recv().await.is_none());
}

#[tokio::test]
async fn a_registered_adapter_cannot_call_unknown_methods_or_register_twice() {
    let mut fixture = Fixture::new();
    let (mut client, _info, _session) = registered(&mut fixture).await;

    client.request(10, "nothing", json!({})).await;
    client.request(11, "register", registration()).await;
    for (expected_id, expected_code) in [
        (10, code::METHOD_NOT_FOUND),
        (11, error_code::NOT_REGISTERED),
    ] {
        match client.recv().await {
            Some(Message::Response {
                id,
                outcome: Err(error),
            }) => {
                assert_eq!(id, json!(expected_id));
                assert_eq!(error.code, expected_code);
            }
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[tokio::test]
async fn several_adapters_register_side_by_side() {
    let mut fixture = Fixture::new();
    let mut keep = Vec::new();
    for index in 0..3u32 {
        let mut client = fixture.client().await;
        let mut params = registration();
        params["pid"] = json!(1000 + index);
        client.request(1, "register", params).await;
        let registering = timeout(WAIT, fixture.server.accept())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(registering.registration().pid, 1000 + index);
        let session = registering.accept(PORT + index as u16);
        assert_eq!(client.registered().await.unwrap(), PORT + index as u16);
        keep.push((client, session));
    }
}

#[tokio::test]
async fn it_works_over_a_real_unix_socket_that_only_the_user_can_reach() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("run").join("adapter.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    assert_eq!(mode(&socket), 0o600);
    assert_eq!(mode(socket.parent().unwrap()), 0o700);
    // Another server on the same socket is refused while this one is alive.
    assert_eq!(
        UnixListener::bind(&socket).unwrap_err().kind(),
        std::io::ErrorKind::AddrInUse
    );

    let mut server = AdapterServer::start(listener, Config::default());
    let mut client = Client::new(tokio::net::UnixStream::connect(&socket).await.unwrap());
    client.request(1, "register", registration()).await;
    let registering = timeout(WAIT, server.accept()).await.unwrap().unwrap();
    let _session = registering.accept(PORT);
    assert_eq!(client.registered().await.unwrap(), PORT);

    drop(server);
    // The socket file goes away with the listener (the accept task is aborted on drop).
    timeout(WAIT, async {
        while socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the socket file is removed");
}

#[tokio::test]
async fn a_socket_file_left_by_a_dead_server_is_replaced() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("adapter.sock");
    // A server that died: the file is there, nobody listens.
    drop(std::os::unix::net::UnixListener::bind(&socket).unwrap());
    assert!(socket.exists());

    let listener = UnixListener::bind(&socket).unwrap();
    assert_eq!(listener.path(), socket);
    assert!(tokio::net::UnixStream::connect(&socket).await.is_ok());
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}
