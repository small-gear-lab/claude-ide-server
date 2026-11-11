//! Tests of the workspaces: the adapter side and the Claude side together, over in-memory streams, with the lock
//! files in a temporary directory.

mod common;

use std::fs;
use std::time::Duration;

use claude_ide_core::adapter::memory::{MemoryConnector, listener};
use claude_ide_core::adapter::{self, RequestError, error_code};
use claude_ide_core::jsonrpc::Message;
use claude_ide_core::lock::{EnvVars, LockContents, LockDirs};
use claude_ide_core::workspace::{Config, Workspace, WorkspaceEvent, WorkspaceServer, socket_path};
use claude_ide_core::ws::AUTH_HEADER;
use common::{Client, WAIT, registration};
use serde_json::json;
use tempfile::TempDir;
use tokio::io::DuplexStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

struct Fixture {
    // Held so that the temporary directory lives as long as the fixture.
    _root: TempDir,
    dirs: LockDirs,
    connector: MemoryConnector,
    server: WorkspaceServer,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let dirs = LockDirs {
            lock_dir: root.path().join("ide"),
            record_dir: root.path().join("run"),
        };
        Self::in_dirs(root, dirs)
    }

    fn in_dirs(root: TempDir, dirs: LockDirs) -> Self {
        let (listener, connector) = listener();
        let server = WorkspaceServer::start(
            listener,
            adapter::Config::default(),
            Config::new(dirs.clone(), std::process::id()),
        );
        Self {
            _root: root,
            dirs,
            connector,
            server,
        }
    }

    async fn client(&self) -> Client<DuplexStream> {
        Client::new(self.connector.connect().await.unwrap())
    }

    fn lock_files(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dirs.lock_dir) else {
            return Vec::new();
        };
        let mut names: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    fn read_lock(&self, port: u16) -> LockContents {
        serde_json::from_slice(&fs::read(self.dirs.lock_dir.join(format!("{port}.lock"))).unwrap())
            .unwrap()
    }
}

async fn registered(fixture: &mut Fixture) -> (Client<DuplexStream>, Workspace, u16) {
    let mut client = fixture.client().await;
    let port = client.register(registration()).await.unwrap();
    let workspace = timeout(WAIT, fixture.server.accept())
        .await
        .unwrap()
        .expect("a workspace");
    (client, workspace, port)
}

async fn claude_connect(port: u16, token: &str) -> Result<(), WsError> {
    let mut request = format!("ws://127.0.0.1:{port}/")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTH_HEADER, token.parse().unwrap());
    tokio_tungstenite::connect_async(request).await.map(|_| ())
}

#[tokio::test]
async fn registering_gives_a_port_a_lock_file_and_a_workspace() {
    let mut fixture = Fixture::new();
    let (_client, mut workspace, port) = registered(&mut fixture).await;

    assert_eq!(workspace.port(), port);
    let registration = workspace.registration();
    assert_eq!(registration.ide_name, "Neovim");
    assert_eq!(registration.pid, 1234);
    assert_eq!(registration.workspace_folders, ["/home/user/project"]);
    assert_eq!(registration.methods, ["getOpenEditors", "openFile"]);

    assert_eq!(fixture.lock_files(), [format!("{port}.lock")]);
    let lock = fixture.read_lock(port);
    assert_eq!(lock.pid, 1234);
    assert_eq!(lock.ide_name, "Neovim");
    assert_eq!(lock.transport, "ws");
    assert_eq!(lock.workspace_folders, ["/home/user/project"]);
    assert_eq!(lock.auth_token.len(), 32);

    // Claude Code can connect with the token from the lock file, and is told about it.
    assert!(claude_connect(port, "wrong").await.is_err());
    claude_connect(port, &lock.auth_token).await.unwrap();
    match timeout(WAIT, workspace.next_event()).await.unwrap() {
        Some(WorkspaceEvent::ClaudeConnected(_)) => {}
        other => panic!("expected a Claude connection, got {other:?}"),
    }
}

#[tokio::test]
async fn a_workspace_that_cannot_be_set_up_is_refused_and_leaves_nothing_behind() {
    let root = TempDir::new().unwrap();
    // The lock directory would have to be created below a regular file.
    let blocker = root.path().join("not-a-directory");
    fs::write(&blocker, b"x").unwrap();
    let dirs = LockDirs {
        lock_dir: blocker.join("ide"),
        record_dir: root.path().join("run"),
    };
    let mut fixture = Fixture::in_dirs(root, dirs);

    let mut client = fixture.client().await;
    let error = client.register(registration()).await.unwrap_err();
    assert_eq!(error.code, error_code::SETUP_FAILED);
    assert!(error.message.contains("could not set up"), "{error}");
    assert!(client.recv().await.is_none());
    assert!(
        timeout(Duration::from_millis(200), fixture.server.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn changed_folders_rewrite_the_lock_file_and_reach_the_events() {
    let mut fixture = Fixture::new();
    let (mut client, mut workspace, port) = registered(&mut fixture).await;

    client
        .notify(
            "workspaceFoldersChanged",
            json!({"workspaceFolders": ["/a", "/b"]}),
        )
        .await;
    match timeout(WAIT, workspace.next_event()).await.unwrap() {
        Some(WorkspaceEvent::Notification { method, .. }) => {
            assert_eq!(method, "workspaceFoldersChanged");
        }
        other => panic!("unexpected {other:?}"),
    }
    assert_eq!(fixture.read_lock(port).workspace_folders, ["/a", "/b"]);
    assert_eq!(workspace.registration().workspace_folders, ["/a", "/b"]);

    // A payload that is not a list of folders changes nothing.
    client
        .notify("workspaceFoldersChanged", json!({"workspaceFolders": []}))
        .await;
    client
        .notify("selectionChanged", json!({"filePath": "/a/x"}))
        .await;
    for expected in ["workspaceFoldersChanged", "selectionChanged"] {
        match timeout(WAIT, workspace.next_event()).await.unwrap() {
            Some(WorkspaceEvent::Notification { method, .. }) => assert_eq!(method, expected),
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(fixture.read_lock(port).workspace_folders, ["/a", "/b"]);
}

#[tokio::test]
async fn when_the_adapter_goes_away_the_lock_file_and_the_port_go_with_it() {
    let mut fixture = Fixture::new();
    let (client, mut workspace, port) = registered(&mut fixture).await;
    let token = fixture.read_lock(port).auth_token;
    let handle = workspace.handle();

    // A request is waiting for an answer that will never come.
    let waiting = tokio::spawn({
        let handle = handle.clone();
        async move { handle.request("getOpenEditors", json!({})).await }
    });
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(client);

    assert!(
        timeout(WAIT, workspace.next_event())
            .await
            .unwrap()
            .is_none()
    );
    assert!(fixture.lock_files().is_empty());
    assert!(claude_connect(port, &token).await.is_err());
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
async fn closing_the_workspace_says_shutdown_and_removes_everything() {
    let mut fixture = Fixture::new();
    let (mut client, workspace, port) = registered(&mut fixture).await;
    let token = fixture.read_lock(port).auth_token;

    timeout(WAIT, workspace.close()).await.unwrap();

    match client.recv().await {
        Some(Message::Notification { method, .. }) => assert_eq!(method, "shutdown"),
        other => panic!("expected shutdown, got {other:?}"),
    }
    assert!(client.recv().await.is_none());
    assert!(fixture.lock_files().is_empty());
    assert!(claude_connect(port, &token).await.is_err());
}

#[tokio::test]
async fn dropping_the_workspace_does_the_same() {
    let mut fixture = Fixture::new();
    let (mut client, workspace, port) = registered(&mut fixture).await;
    let token = fixture.read_lock(port).auth_token;
    drop(workspace);
    match client.recv().await {
        Some(Message::Notification { method, .. }) => assert_eq!(method, "shutdown"),
        other => panic!("expected shutdown, got {other:?}"),
    }
    assert!(client.recv().await.is_none());
    assert!(fixture.lock_files().is_empty());
    assert!(claude_connect(port, &token).await.is_err());
}

#[tokio::test]
async fn requests_reach_the_adapter_through_the_workspace() {
    let mut fixture = Fixture::new();
    let (mut client, workspace, _port) = registered(&mut fixture).await;
    let handle = workspace.handle();

    let call =
        tokio::spawn(async move { handle.request("openFile", json!({"filePath": "/a"})).await });
    let id = match client.recv().await {
        Some(Message::Request { id, method, .. }) => {
            assert_eq!(method, "openFile");
            id
        }
        other => panic!("expected a request, got {other:?}"),
    };
    client
        .send(Message::Response {
            id,
            outcome: Ok(json!({"languageId": "lua"})),
        })
        .await;
    assert_eq!(call.await.unwrap(), Ok(json!({"languageId": "lua"})));
}

#[tokio::test]
async fn several_adapters_each_get_their_own_port_and_lock_file() {
    let mut fixture = Fixture::new();
    let mut ports = Vec::new();
    let mut keep = Vec::new();
    for index in 0..3 {
        let mut client = fixture.client().await;
        let mut params = registration();
        params["pid"] = json!(1000 + index);
        ports.push(client.register(params).await.unwrap());
        keep.push((
            client,
            timeout(WAIT, fixture.server.accept())
                .await
                .unwrap()
                .unwrap(),
        ));
    }
    ports.sort();
    ports.dedup();
    assert_eq!(ports.len(), 3);
    assert_eq!(fixture.lock_files().len(), 3);
    drop(keep);
}

#[tokio::test]
async fn a_lock_file_left_by_a_dead_server_is_removed_when_the_next_one_starts() {
    use claude_ide_core::lock::{LockFile, TRANSPORT_WS};

    let root = TempDir::new().unwrap();
    let dirs = LockDirs {
        lock_dir: root.path().join("ide"),
        record_dir: root.path().join("run"),
    };
    // An earlier server wrote a lock file and died (nothing ran Drop).
    let leftover = LockFile::create(
        &dirs,
        41999,
        LockContents {
            pid: 1,
            workspace_folders: vec!["/old".to_owned()],
            ide_name: "Neovim".to_owned(),
            transport: TRANSPORT_WS.to_owned(),
            running_in_windows: false,
            auth_token: "old".to_owned(),
        },
        7777,
    )
    .unwrap();
    let path = leftover.path().to_owned();
    std::mem::forget(leftover);

    let start = |is_alive: fn(u32) -> bool| {
        let (listener, _connector) = listener();
        WorkspaceServer::start(
            listener,
            adapter::Config::default(),
            Config::new(dirs.clone(), std::process::id()).with_liveness(is_alive),
        )
    };

    // While the recorded server is still alive, its file is left alone.
    let server = start(|_| true);
    assert!(server.stale_removed().is_empty());
    assert!(path.exists());
    drop(server);

    // Once it is gone, the next start removes the file.
    let server = start(|pid| pid != 7777);
    assert_eq!(server.stale_removed(), std::slice::from_ref(&path));
    assert!(!path.exists());
}

fn lock_files_in(dirs: &LockDirs) -> Vec<String> {
    fs::read_dir(&dirs.lock_dir)
        .map(|entries| {
            entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default()
}

async fn expect_shutdown_then_end(client: &mut Client<DuplexStream>) {
    match client.recv().await {
        Some(Message::Notification { method, .. }) => assert_eq!(method, "shutdown"),
        other => panic!("expected shutdown, got {other:?}"),
    }
    assert!(client.recv().await.is_none());
}

#[tokio::test]
async fn the_connections_of_claude_are_closed_when_the_adapter_goes_away() {
    let mut fixture = Fixture::new();
    let (client, mut workspace, port) = registered(&mut fixture).await;
    let token = fixture.read_lock(port).auth_token;

    // Claude Code connects, and the connection is handed to whoever owns the workspace.
    let mut request = format!("ws://127.0.0.1:{port}/")
        .into_client_request()
        .unwrap();
    request
        .headers_mut()
        .insert(AUTH_HEADER, token.parse().unwrap());
    let (mut claude, _) = tokio_tungstenite::connect_async(request).await.unwrap();
    let mut connection = match timeout(WAIT, workspace.next_event()).await.unwrap() {
        Some(WorkspaceEvent::ClaudeConnected(connection)) => connection,
        other => panic!("expected a Claude connection, got {other:?}"),
    };

    drop(client); // the adapter goes away

    // The owner still holds the connection, but it has been closed under it.
    assert!(timeout(WAIT, connection.recv()).await.unwrap().is_none());
    match timeout(WAIT, futures_util::StreamExt::next(&mut claude))
        .await
        .unwrap()
    {
        Some(Ok(tokio_tungstenite::tungstenite::Message::Close(Some(frame)))) => {
            assert_eq!(u16::from(frame.code), 1001);
        }
        other => panic!("expected a close frame, got {other:?}"),
    }
}

#[tokio::test]
async fn stopping_the_server_tells_every_adapter_shutdown_and_removes_everything() {
    let Fixture {
        dirs,
        connector,
        mut server,
        ..
    } = Fixture::new();
    let mut adapters = Vec::new();
    for index in 0..2 {
        let mut client = Client::new(connector.connect().await.unwrap());
        let mut params = registration();
        params["pid"] = json!(1000 + index);
        client.register(params).await.unwrap();
        let workspace = timeout(WAIT, server.accept()).await.unwrap().unwrap();
        adapters.push((client, workspace));
    }
    assert_eq!(lock_files_in(&dirs).len(), 2);

    timeout(WAIT, server.shutdown()).await.unwrap();

    assert!(lock_files_in(&dirs).is_empty());
    for (client, workspace) in &mut adapters {
        expect_shutdown_then_end(client).await;
        assert!(
            timeout(WAIT, workspace.next_event())
                .await
                .unwrap()
                .is_none()
        );
    }
}

#[tokio::test]
async fn dropping_the_server_stops_its_workspaces_too() {
    let Fixture {
        dirs,
        connector,
        mut server,
        ..
    } = Fixture::new();
    let mut client = Client::new(connector.connect().await.unwrap());
    client.register(registration()).await.unwrap();
    let mut workspace = timeout(WAIT, server.accept()).await.unwrap().unwrap();

    drop(server);

    expect_shutdown_then_end(&mut client).await;
    assert!(
        timeout(WAIT, workspace.next_event())
            .await
            .unwrap()
            .is_none()
    );
    assert!(lock_files_in(&dirs).is_empty());
}

#[test]
fn the_adapter_socket_goes_into_the_runtime_directory() {
    let env = |xdg: Option<&str>| EnvVars {
        claude_config_dir: None,
        home: Some("/home/u".into()),
        xdg_runtime_dir: xdg.map(Into::into),
        uid: 1000,
    };
    assert_eq!(
        socket_path(&env(Some("/run/user/1000"))),
        std::path::PathBuf::from("/run/user/1000/claude-ide-server/adapter.sock")
    );
    assert_eq!(
        socket_path(&env(None)),
        std::env::temp_dir()
            .join("claude-ide-server-1000")
            .join("adapter.sock")
    );
}
