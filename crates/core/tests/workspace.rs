//! Tests of the workspaces: the adapter side and the Claude side together, over in-memory streams, with the lock
//! files in a temporary directory.

mod common;

use std::fs;
use std::time::Duration;

use claude_ide_core::adapter::memory::listener;
use claude_ide_core::adapter::{self, RequestError, error_code};
use claude_ide_core::jsonrpc::Message;
use claude_ide_core::lock::{EnvVars, LockContents, LockDirs};
use claude_ide_core::workspace::{Config, WorkspaceEvent, WorkspaceServer, socket_path};
use common::claude::MockClaude;
use common::fixture::{Fixture, claude_connect, registered};
use common::{Client, WAIT, registration};
use serde_json::json;
use tempfile::TempDir;
use tokio::io::DuplexStream;
use tokio::time::timeout;

#[tokio::test]
async fn registering_gives_a_port_a_lock_file_and_a_workspace() {
    let mut fixture = Fixture::new();
    let (_client, workspace, port) = registered(&mut fixture).await;

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

    // Claude Code can connect with the token from the lock file, and then is a client of the workspace.
    assert!(claude_connect(port, "wrong").await.is_err());
    let mut claude = MockClaude::connect(port, &lock.auth_token).await;
    claude.handshake(4321).await;
    claude.tool_names().await; // by now the handshake has been served
    let clients = workspace.claude_clients();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].name, "claude-code");
    assert_eq!(clients[0].version, "2.1.322");
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
        .notify("diagnosticsChanged", json!({"uris": []}))
        .await;
    for expected in ["workspaceFoldersChanged", "diagnosticsChanged"] {
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
    let (client, workspace, port) = registered(&mut fixture).await;
    let token = fixture.read_lock(port).auth_token;

    let mut claude = MockClaude::connect(port, &token).await;
    claude.handshake(4321).await;
    claude.tool_names().await;
    assert_eq!(workspace.claude_clients().len(), 1);

    drop(client); // the adapter goes away

    // Claude Code is told that the server is going away, and the client leaves the workspace.
    assert_eq!(claude.close_code().await, Some(1001));
    timeout(WAIT, async {
        while !workspace.claude_clients().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the client is removed");
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

/// On Windows the default is a named pipe, which has no directory: it has the user in its name, so that two users of one
/// machine do not meet.
#[cfg(windows)]
#[test]
fn the_adapter_pipe_has_the_user_in_its_name() {
    let env = |xdg: Option<&str>| EnvVars {
        claude_config_dir: None,
        home: Some(r"C:\Users\u".into()),
        xdg_runtime_dir: xdg.map(Into::into),
        socket: None,
        user: "S-1-5-21-1-2-3-1001".into(),
    };
    let expected = std::path::PathBuf::from(r"\\.\pipe\claude-ide-server-S-1-5-21-1-2-3-1001");
    assert_eq!(socket_path(&env(None)), expected);
    // The runtime directory does not matter to a pipe.
    assert_eq!(socket_path(&env(Some(r"C:\run"))), expected);
}

#[cfg(unix)]
#[test]
fn the_adapter_socket_goes_into_the_runtime_directory() {
    let env = |xdg: Option<&str>| EnvVars {
        claude_config_dir: None,
        home: Some("/home/u".into()),
        xdg_runtime_dir: xdg.map(Into::into),
        socket: None,
        user: "1000".into(),
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

/// One place for the server and for the adapters that read the variable, whatever the system's default is.
#[test]
fn the_socket_can_be_set_by_the_environment() {
    let env = EnvVars {
        claude_config_dir: None,
        home: None,
        xdg_runtime_dir: Some("/run/user/1000".into()),
        socket: Some("/somewhere/else".into()),
        user: "1000".into(),
    };
    assert_eq!(
        socket_path(&env),
        std::path::PathBuf::from("/somewhere/else")
    );
}

#[tokio::test]
async fn a_source_of_tokens_that_fails_gives_a_refusal_and_leaves_nothing_behind() {
    let root = TempDir::new().unwrap();
    let dirs = LockDirs {
        lock_dir: root.path().join("ide"),
        record_dir: root.path().join("run"),
    };
    let (listener, connector) = listener();
    let mut server = WorkspaceServer::start(
        listener,
        adapter::Config::default(),
        Config::new(dirs.clone(), std::process::id())
            .with_token_source(|| Err(std::io::Error::other("no random numbers today"))),
    );

    let mut client = Client::new(connector.connect().await.unwrap());
    let error = client.register(registration()).await.unwrap_err();
    assert_eq!(error.code, error_code::SETUP_FAILED);
    assert!(error.message.contains("no random numbers today"), "{error}");
    assert!(client.recv().await.is_none());
    assert!(lock_files_in(&dirs).is_empty());
    assert!(
        timeout(Duration::from_millis(200), server.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn a_token_that_is_known_in_advance_ends_up_in_the_lock_file_and_works() {
    let root = TempDir::new().unwrap();
    let dirs = LockDirs {
        lock_dir: root.path().join("ide"),
        record_dir: root.path().join("run"),
    };
    let (listener, connector) = listener();
    let mut server = WorkspaceServer::start(
        listener,
        adapter::Config::default(),
        Config::new(dirs.clone(), std::process::id())
            .with_token_source(|| Ok("0123456789abcdef0123456789abcdef".to_owned())),
    );

    let mut client = Client::new(connector.connect().await.unwrap());
    let port = client.register(registration()).await.unwrap();
    let _workspace = timeout(WAIT, server.accept()).await.unwrap().unwrap();

    let lock: LockContents =
        serde_json::from_slice(&fs::read(dirs.lock_dir.join(format!("{port}.lock"))).unwrap())
            .unwrap();
    assert_eq!(lock.auth_token, "0123456789abcdef0123456789abcdef");
    claude_connect(port, "0123456789abcdef0123456789abcdef")
        .await
        .unwrap();
}

/// The owner of a workspace that does not read the events does not stall it: the events are dropped, and the workspace
/// goes on serving what the adapter says.
#[tokio::test]
async fn events_that_nobody_reads_are_dropped_and_do_not_stall_the_workspace() {
    let mut fixture = Fixture::new();
    let (mut client, workspace, port) = registered(&mut fixture).await;

    // More than the queue of events holds, and nobody calls next_event().
    for _ in 0..600 {
        client
            .notify("diagnosticsChanged", json!({"uris": []}))
            .await;
    }
    // The workspace is not stuck on them: it still applies what the adapter reports next.
    client
        .notify(
            "workspaceFoldersChanged",
            json!({"workspaceFolders": ["/srv/app"]}),
        )
        .await;
    timeout(WAIT, async {
        while fixture.read_lock(port).workspace_folders != ["/srv/app"] {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the change of folders is applied");
    assert_eq!(workspace.registration().workspace_folders, ["/srv/app"]);
    assert!(
        workspace.dropped_events() > 0,
        "the events that were not read were dropped, not waited for"
    );
}
