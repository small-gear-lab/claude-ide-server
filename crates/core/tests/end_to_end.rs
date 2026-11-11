//! The whole path, with nothing in memory: an adapter on a real Unix socket (a named pipe on Windows) registers, Claude Code connects with the
//! token that it reads from the lock file on disk, lists the tools, calls one, and everything is cleaned up when the
//! adapter goes away.

mod common;

use std::fs;
use std::time::Duration;

use claude_ide_core::adapter;
use claude_ide_core::jsonrpc::Message;
use claude_ide_core::lock::{EnvVars, LockDirs};
use claude_ide_core::workspace::{Config, WorkspaceServer, socket_path};
use common::claude::MockClaude;
use common::shape::assert_shape;
use common::{Client, WAIT, connect_to_adapters, registration};
use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::time::timeout;

#[tokio::test]
async fn register_connect_list_call_disconnect_and_clean_up() {
    // Everything under one temporary directory, found the way a real run finds it: from the environment.
    let root = TempDir::new().unwrap();
    let env = EnvVars {
        claude_config_dir: Some(root.path().join("claude")),
        home: None,
        xdg_runtime_dir: Some(root.path().join("run")),
        socket: None,
        user: "1000".into(),
    };
    let dirs = LockDirs::from_env(&env).unwrap();
    let socket = socket_path(&env);

    let listener = adapter::NativeListener::bind(&socket).unwrap();
    let mut server = WorkspaceServer::start(
        listener,
        adapter::Config::default(),
        Config::new(dirs.clone(), std::process::id()),
    );

    // 1. Register: the adapter connects to the socket and is given a port.
    let mut adapter = Client::new(connect_to_adapters(&socket).await.unwrap());
    let port = adapter.register(registration()).await.unwrap();
    let workspace = timeout(WAIT, server.accept()).await.unwrap().unwrap();
    assert_eq!(workspace.port(), port);

    // 2. Connect: Claude Code reads the lock file, as it does, and connects with the token in it.
    let lock_path = dirs.lock_dir.join(format!("{port}.lock"));
    let lock: Value = serde_json::from_slice(&fs::read(&lock_path).unwrap()).unwrap();
    assert_shape(&lock, "lock_file.shape.json");
    let mut claude = MockClaude::connect(port, lock["authToken"].as_str().unwrap()).await;
    let initialized = claude.handshake(4321).await;
    assert_shape(&initialized["result"], "initialize_result.shape.json");

    // 3. List the tools.
    let listed = claude.request("tools/list", json!({})).await;
    assert_shape(&listed["result"], "tools_list_result.shape.json");
    let names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["openFile", "getOpenEditors", "getWorkspaceFolders"]);

    // 4. Call one tool: it reaches the adapter, and the answer comes back.
    let call = claude.request(
        "tools/call",
        json!({"name": "getOpenEditors", "arguments": {}}),
    );
    let answer_the_request = async {
        match adapter.recv().await {
            Some(Message::Request { id, method, .. }) => {
                assert_eq!(method, "getOpenEditors");
                adapter
                    .send(Message::Response {
                        id,
                        outcome: Ok(json!({"tabs": [{
                            "uri": "file:///a.lua",
                            "isActive": true,
                            "label": "a.lua",
                            "languageId": "lua",
                            "isDirty": false,
                        }]})),
                    })
                    .await;
            }
            other => panic!("expected a request, got {other:?}"),
        }
    };
    let (called, ()) = tokio::join!(call, answer_the_request);
    assert_shape(&called["result"], "tool_call_result.shape.json");
    assert_eq!(called["result"]["isError"], false);
    let text = called["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap()["tabs"][0]["uri"],
        "file:///a.lua"
    );

    // 5. Disconnect: the adapter goes away.
    drop(adapter);

    // 6. Clean up: Claude Code is told, and the lock file, the port and the client are gone.
    assert_eq!(claude.close_code().await, Some(1001));
    timeout(WAIT, async {
        while lock_path.exists() || !workspace.claude_clients().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the lock file and the client are gone");
    assert!(
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_err(),
        "the port is closed"
    );

    // And stopping the server removes the socket.
    server.shutdown().await;
    timeout(WAIT, async {
        while socket.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the socket file is removed");
}
