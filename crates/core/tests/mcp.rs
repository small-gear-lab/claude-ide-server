//! Tests of the MCP side, with a stand-in for Claude Code's client on one end and a stand-in for an adapter on the
//! other, and the workspace between them.

mod common;

use std::time::Duration;

use claude_ide_core::adapter::PROTOCOL_VERSION;
use claude_ide_core::jsonrpc::{Message, RpcError};
use claude_ide_core::workspace::Workspace;
use common::claude::MockClaude;
use common::fixture::{Fixture, registered};
use common::{Client, WAIT};
use serde_json::{Value, json};
use tokio::io::DuplexStream;
use tokio::time::timeout;

/// An adapter that declared `getOpenEditors` and `openFile` is registered, and Claude Code has connected to it and
/// completed the handshake.
struct Scene {
    _fixture: Fixture,
    adapter: Client<DuplexStream>,
    workspace: Workspace,
    claude: MockClaude,
}

async fn scene_in(mut fixture: Fixture) -> Scene {
    let (adapter, workspace, port) = registered(&mut fixture).await;
    let token = fixture.read_lock(port).auth_token;
    let mut claude = MockClaude::connect(port, &token).await;
    claude.handshake(4321).await;
    claude.tool_names().await; // the handshake is done when this is answered
    Scene {
        _fixture: fixture,
        adapter,
        workspace,
        claude,
    }
}

async fn scene() -> Scene {
    scene_in(Fixture::new()).await
}

/// The adapter answers the next request that it gets, and the request is returned.
async fn answer_next_request(
    adapter: &mut Client<DuplexStream>,
    outcome: Result<Value, RpcError>,
) -> (String, Value) {
    match adapter.recv().await {
        Some(Message::Request { id, method, params }) => {
            adapter.send(Message::Response { id, outcome }).await;
            (method, params)
        }
        other => panic!("expected a request, got {other:?}"),
    }
}

/// `tools/call` for a tool, while the adapter answers the one request it should get.
async fn call_tool(
    scene: &mut Scene,
    name: &str,
    arguments: Value,
    outcome: Result<Value, RpcError>,
) -> (Value, (String, Value)) {
    let call = scene.claude.request(
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    );
    let (answer, request) = tokio::join!(call, answer_next_request(&mut scene.adapter, outcome));
    (answer, request)
}

#[tokio::test]
async fn the_handshake_negotiates_the_version_and_says_what_the_server_is() {
    let mut fixture = Fixture::new();
    let (_adapter, _workspace, port) = registered(&mut fixture).await;
    let token = fixture.read_lock(port).auth_token;
    let mut claude = MockClaude::connect(port, &token).await;

    let answer = claude.handshake(4321).await;

    assert_eq!(answer["result"]["protocolVersion"], "2025-11-25");
    assert!(answer["result"]["capabilities"]["tools"].is_object());
    assert_eq!(answer["result"]["serverInfo"]["name"], "claude-ide-server");
    assert_eq!(
        answer["result"]["serverInfo"]["version"],
        env!("CARGO_PKG_VERSION")
    );
}

#[tokio::test]
async fn only_the_tools_that_can_be_run_are_listed() {
    let mut scene = scene().await;
    // The adapter declared getOpenEditors and openFile; the server answers getWorkspaceFolders itself.
    assert_eq!(
        scene.claude.tool_names().await,
        ["openFile", "getOpenEditors", "getWorkspaceFolders"]
    );

    let answer = scene.claude.request("tools/list", json!({})).await;
    for tool in answer["result"]["tools"].as_array().unwrap() {
        assert!(!tool["description"].as_str().unwrap().is_empty());
        assert_eq!(tool["inputSchema"]["type"], "object");
    }
}

#[tokio::test]
async fn a_tool_call_goes_to_the_adapter_and_its_answer_comes_back_as_text() {
    let mut scene = scene().await;
    let (answer, (method, params)) = call_tool(
        &mut scene,
        "openFile",
        json!({"filePath": "/home/user/project/a.lua"}),
        Ok(json!({"languageId": "lua", "lineCount": 3})),
    )
    .await;

    assert_eq!(method, "openFile");
    assert_eq!(params, json!({"filePath": "/home/user/project/a.lua"}));
    assert_eq!(answer["result"]["isError"], false);
    assert_eq!(answer["result"]["content"][0]["type"], "text");
    let text = answer["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(text).unwrap(),
        json!({"languageId": "lua", "lineCount": 3})
    );
}

#[tokio::test]
async fn an_answer_that_is_a_string_is_the_text_as_it_is() {
    let mut scene = scene().await;
    let (answer, _) = call_tool(
        &mut scene,
        "getOpenEditors",
        json!({}),
        Ok(json!("FILE_SAVED")),
    )
    .await;
    assert_eq!(answer["result"]["content"][0]["text"], "FILE_SAVED");
    assert_eq!(answer["result"]["isError"], false);
}

#[tokio::test]
async fn a_failed_tool_call_is_an_answer_with_the_error_flag() {
    let mut scene = scene().await;
    let (answer, _) = call_tool(
        &mut scene,
        "openFile",
        json!({"filePath": "/nope"}),
        Err(RpcError::new(-1, "no such file")),
    )
    .await;
    assert_eq!(answer["result"]["isError"], true);
    assert_eq!(answer["result"]["content"][0]["text"], "no such file");
    assert!(answer.get("error").is_none());
}

#[tokio::test]
async fn an_adapter_that_does_not_answer_gives_a_failed_call_not_a_hang() {
    let mut scene = scene_in(Fixture::with_request_timeout(Duration::from_millis(300))).await;
    let answer = scene
        .claude
        .request(
            "tools/call",
            json!({"name": "openFile", "arguments": {"filePath": "/a"}}),
        )
        .await;
    assert_eq!(answer["result"]["isError"], true);
    assert!(
        answer["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("did not answer in time")
    );
}

#[tokio::test]
async fn a_tool_that_is_not_available_is_an_error_of_the_protocol() {
    let mut scene = scene().await;
    // saveDocument exists, but the adapter did not declare it; nothing is sent to the adapter.
    let answer = scene
        .claude
        .request(
            "tools/call",
            json!({"name": "saveDocument", "arguments": {"filePath": "/a"}}),
        )
        .await;
    assert_eq!(answer["error"]["code"], -32602);
    assert!(
        answer["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown tool")
    );

    let answer = scene
        .claude
        .request("tools/call", json!({"name": "noSuchTool", "arguments": {}}))
        .await;
    assert_eq!(answer["error"]["code"], -32602);
    assert!(
        timeout(Duration::from_millis(200), scene.adapter.recv())
            .await
            .is_err(),
        "the adapter was not asked"
    );
}

#[tokio::test]
async fn the_folders_come_from_the_registration_and_follow_its_changes() {
    let mut scene = scene().await;
    let folders = |answer: &Value| -> Value {
        let text = answer["result"]["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    };

    let answer = scene
        .claude
        .request(
            "tools/call",
            json!({"name": "getWorkspaceFolders", "arguments": {}}),
        )
        .await;
    let before = folders(&answer);
    assert_eq!(before["success"], true);
    assert_eq!(before["rootPath"], "/home/user/project");
    assert_eq!(before["folders"][0]["name"], "project");
    assert_eq!(before["folders"][0]["uri"], "file:///home/user/project");

    scene
        .adapter
        .notify(
            "workspaceFoldersChanged",
            json!({"workspaceFolders": ["/srv/app", "/srv/lib"]}),
        )
        .await;
    // The change has been applied when the workspace has passed it on.
    timeout(WAIT, scene.workspace.next_event())
        .await
        .unwrap()
        .unwrap();

    let answer = scene
        .claude
        .request(
            "tools/call",
            json!({"name": "getWorkspaceFolders", "arguments": {}}),
        )
        .await;
    let after = folders(&answer);
    assert_eq!(after["rootPath"], "/srv/app");
    assert_eq!(after["folders"][1]["path"], "/srv/lib");
}

#[tokio::test]
async fn the_client_is_known_by_its_name_its_version_and_the_pid_from_ide_connected() {
    let scene = scene().await;
    // ide_connected is a notification: it may be handled a moment after the answer to tools/list.
    timeout(WAIT, async {
        while scene
            .workspace
            .claude_clients()
            .iter()
            .all(|client| client.pid.is_none())
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the pid arrives");
    let clients = scene.workspace.claude_clients();
    assert_eq!(clients.len(), 1);
    assert_eq!(clients[0].name, "claude-code");
    assert_eq!(clients[0].version, "2.1.322");
    assert_eq!(clients[0].pid, Some(4321));
}

#[tokio::test]
async fn a_notification_reaches_every_connected_client() {
    let mut scene = scene().await;
    let port = scene.workspace.port();
    let token = scene._fixture.read_lock(port).auth_token;
    let mut second = MockClaude::connect(port, &token).await;
    second.handshake(4322).await;
    second.tool_names().await;
    assert_eq!(scene.workspace.claude_clients().len(), 2);

    let params = json!({"filePath": "/a.lua", "text": "", "selection": {"isEmpty": true}});
    scene
        .workspace
        .notify_claude("selection_changed", params.clone())
        .await;

    for claude in [&mut scene.claude, &mut second] {
        let message = claude.recv_json().await.expect("a notification");
        assert_eq!(message["method"], "selection_changed");
        assert_eq!(message["params"], params);
        assert!(message.get("id").is_none());
    }
}

#[tokio::test]
async fn ping_is_answered_unknown_methods_are_refused_and_garbage_is_ignored() {
    let mut scene = scene().await;

    let answer = scene.claude.request("ping", json!({})).await;
    assert_eq!(answer["result"], json!({}));

    let answer = scene.claude.request("no/such/method", json!({})).await;
    assert_eq!(answer["error"]["code"], -32601);

    // Something that is not JSON-RPC at all does not end the connection.
    scene.claude.send_raw("this is not json").await;
    assert_eq!(
        scene.claude.tool_names().await,
        ["openFile", "getOpenEditors", "getWorkspaceFolders"]
    );
}

#[tokio::test]
async fn a_client_that_leaves_is_forgotten() {
    let scene = scene().await;
    let Scene {
        claude, workspace, ..
    } = scene;
    assert_eq!(workspace.claude_clients().len(), 1);
    drop(claude);
    timeout(WAIT, async {
        while !workspace.claude_clients().is_empty() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the client is forgotten");
}

#[test]
fn the_protocol_version_of_the_adapter_side_is_unrelated_to_the_one_of_mcp() {
    // Two different things that are both called a version: the adapter protocol's is a small integer.
    assert_eq!(PROTOCOL_VERSION, 1);
}
