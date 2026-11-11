//! The read-only tools and the selection notifications: what an adapter reports is checked, rewritten with the crate's
//! own types, and passed on to Claude Code in the shapes that it expects.

mod common;

use std::time::Duration;

use claude_ide_core::workspace::WorkspaceEvent;
use common::WAIT;
use common::claude::MockClaude;
use common::scene::{Scene, call_tool, result_json, scene_declaring};
use common::shape::assert_shape;
use serde_json::{Value, json};
use tokio::time::timeout;

const READ_ONLY: [&str; 4] = [
    "getOpenEditors",
    "getCurrentSelection",
    "getLatestSelection",
    "getDiagnostics",
];

fn selection(text: &str, start: (u32, u32), end: (u32, u32)) -> Value {
    json!({
        "filePath": "/home/user/project/a.lua",
        "text": text,
        "selection": {
            "start": {"line": start.0, "character": start.1},
            "end": {"line": end.0, "character": end.1},
        },
    })
}

#[tokio::test]
async fn the_read_only_tools_are_listed_with_the_folders_tool_that_the_server_answers_itself() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    assert_eq!(
        scene.claude.tool_names().await,
        [
            "getOpenEditors",
            "getCurrentSelection",
            "getLatestSelection",
            "getWorkspaceFolders",
            "getDiagnostics"
        ]
    );
}

#[tokio::test]
async fn the_open_editors_are_rewritten_through_the_types() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    let tabs = json!({"tabs": [
        {"uri": "file:///a.lua", "isActive": true, "label": "a.lua", "languageId": "lua", "isDirty": false},
        {"uri": "file:///b.rs", "isActive": false, "label": "b.rs", "languageId": "rust", "isDirty": true},
    ]});
    let (answer, (method, _)) =
        call_tool(&mut scene, "getOpenEditors", json!({}), Ok(tabs.clone())).await;

    assert_eq!(method, "getOpenEditors");
    assert_eq!(answer["result"]["isError"], false);
    assert_eq!(result_json(&answer), tabs);
    assert_shape(&result_json(&answer), "get_open_editors_result.shape.json");
}

#[tokio::test]
async fn a_selection_is_answered_with_its_position_and_text() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    for tool in ["getCurrentSelection", "getLatestSelection"] {
        let (answer, (method, _)) = call_tool(
            &mut scene,
            tool,
            json!({}),
            Ok(selection("日本語", (3, 0), (3, 3))),
        )
        .await;
        assert_eq!(method, tool);
        let result = result_json(&answer);
        // The VS Code extension writes `success` for the current selection, and not for the latest one.
        if tool == "getCurrentSelection" {
            assert_shape(&result, "get_selection_result.shape.json");
        } else {
            assert_shape(&result, "get_latest_selection_result.shape.json");
            assert!(result.get("success").is_none(), "{result}");
        }
        assert_eq!(result["fileUrl"], "file:///home/user/project/a.lua");
        assert_eq!(result["text"], "日本語");
        assert_eq!(result["selection"]["end"]["character"], 3);
        assert_eq!(result["selection"]["isEmpty"], false);
    }
}

#[tokio::test]
async fn no_selection_is_an_answer_not_an_error() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    let (answer, _) = call_tool(
        &mut scene,
        "getCurrentSelection",
        json!({}),
        Ok(json!({"success": false, "message": "No active editor found"})),
    )
    .await;
    assert_eq!(answer["result"]["isError"], false);
    let result = result_json(&answer);
    assert_shape(&result, "get_selection_none_result.shape.json");
    assert_eq!(result["message"], "No active editor found");

    // Without a message, the usual one.
    let (answer, _) = call_tool(
        &mut scene,
        "getLatestSelection",
        json!({}),
        Ok(json!({"success": false})),
    )
    .await;
    assert_eq!(result_json(&answer)["message"], "No selection available");
}

#[tokio::test]
async fn the_diagnostics_are_asked_for_by_uri_and_rewritten_through_the_types() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    let files = json!([{
        "uri": "file:///a.lua",
        "diagnostics": [
            {"message": "unused", "severity": "Warning", "source": "lua_ls",
             "range": {"start": {"line": 1, "character": 2}, "end": {"line": 1, "character": 5}}},
        ],
    }]);
    let (answer, (method, params)) = call_tool(
        &mut scene,
        "getDiagnostics",
        json!({"uri": "file:///a.lua"}),
        Ok(files.clone()),
    )
    .await;
    assert_eq!(method, "getDiagnostics");
    assert_eq!(params, json!({"uri": "file:///a.lua"}));
    assert_eq!(result_json(&answer), files);
    assert_shape(&result_json(&answer), "get_diagnostics_result.shape.json");

    // No file has anything: an empty list, not a failure.
    let (answer, _) = call_tool(&mut scene, "getDiagnostics", json!({}), Ok(json!([]))).await;
    assert_eq!(answer["result"]["isError"], false);
    assert_eq!(result_json(&answer), json!([]));
}

#[tokio::test]
async fn a_malformed_answer_is_a_failed_call_that_says_where() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    let cases = [
        (
            "getOpenEditors",
            json!({"tabs": [{"uri": "x"}]}),
            "$.tabs[0].isActive",
        ),
        ("getOpenEditors", json!({"tabs": "none"}), "$.tabs"),
        ("getCurrentSelection", json!({"filePath": "/a"}), "$.text"),
        ("getLatestSelection", json!("a string"), "$"),
        ("getDiagnostics", json!({"not": "a list"}), "$"),
        (
            "getDiagnostics",
            json!([{"uri": "x", "diagnostics": [{"message": "m", "severity": "Fatal", "range": {}}]}]),
            "$[0].diagnostics[0].severity",
        ),
    ];
    for (tool, malformed, path) in cases {
        let (answer, _) = call_tool(&mut scene, tool, json!({}), Ok(malformed.clone())).await;
        assert!(
            answer.get("error").is_none(),
            "{tool}: a failed call is still an answer"
        );
        assert_eq!(answer["result"]["isError"], true, "{tool}: {malformed}");
        let text = answer["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains(path), "{tool}: {text}");
        assert!(text.contains("is not valid"), "{tool}: {text}");
    }
}

/// The adapter reports a selection; Claude Code is told, in the shape that it expects.
#[tokio::test]
async fn a_selection_that_the_adapter_reports_reaches_claude_as_selection_changed() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    scene
        .adapter
        .notify(
            "selectionChanged",
            selection("local x = 1", (0, 0), (0, 11)),
        )
        .await;

    let message = scene.claude.recv_json().await.expect("a notification");
    assert_eq!(message["method"], "selection_changed");
    assert!(message.get("id").is_none());
    assert_shape(&message["params"], "selection_changed.shape.json");
    let params = &message["params"];
    assert_eq!(params["filePath"], "/home/user/project/a.lua");
    assert_eq!(params["fileUrl"], "file:///home/user/project/a.lua");
    assert_eq!(params["text"], "local x = 1");
    assert_eq!(params["selection"]["isEmpty"], false);

    // The workspace's owner sees it as well, after it was passed on.
    match timeout(WAIT, scene.workspace.next_event()).await.unwrap() {
        Some(WorkspaceEvent::Notification { method, .. }) => assert_eq!(method, "selectionChanged"),
        other => panic!("unexpected {other:?}"),
    }
}

#[tokio::test]
async fn a_cursor_without_a_selection_is_an_empty_selection() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    scene
        .adapter
        .notify("selectionChanged", selection("", (7, 4), (7, 4)))
        .await;
    let message = scene.claude.recv_json().await.expect("a notification");
    assert_eq!(message["params"]["selection"]["isEmpty"], true);
    assert_eq!(message["params"]["text"], "");
}

#[tokio::test]
async fn a_mentioned_file_reaches_claude_as_at_mentioned() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    scene
        .adapter
        .notify(
            "atMention",
            json!({"filePath": "/home/user/project/a.lua", "lineStart": 4, "lineEnd": 9}),
        )
        .await;
    let message = scene.claude.recv_json().await.expect("a notification");
    assert_eq!(message["method"], "at_mentioned");
    assert_shape(&message["params"], "at_mentioned.shape.json");
    assert_eq!(message["params"]["lineStart"], 4);
    assert_eq!(message["params"]["lineEnd"], 9);
}

#[tokio::test]
async fn changed_diagnostics_reach_claude_as_an_event_and_are_not_told_to_one_that_comes_later() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    scene
        .adapter
        .notify(
            "diagnosticsChanged",
            json!({"uris": ["file:///home/user/project/a.lua", "file:///home/user/project/b.lua"]}),
        )
        .await;
    scene
        .adapter
        .notify(
            "diagnosticsChanged",
            json!({"uris": ["file:///home/user/project/c.lua"]}),
        )
        .await;

    // Both come, in order, and neither replaces the other: they name different files.
    for expected in [
        json!({"uris": ["file:///home/user/project/a.lua", "file:///home/user/project/b.lua"]}),
        json!({"uris": ["file:///home/user/project/c.lua"]}),
    ] {
        let message = scene.claude.recv_json().await.expect("a notification");
        assert_eq!(message["method"], "diagnostics_changed");
        assert!(message.get("id").is_none());
        assert_shape(&message["params"], "diagnostics_changed.shape.json");
        assert_eq!(message["params"], expected);
    }

    // What happened before a Claude Code came is not told to it: it asks for the diagnostics.
    let port = scene.workspace.port();
    let token = scene.fixture.read_lock(port).auth_token;
    let mut later = MockClaude::connect(port, &token).await;
    later.handshake(5000).await;
    assert!(
        timeout(Duration::from_millis(300), later.recv_json())
            .await
            .is_err(),
        "nothing is replayed"
    );
}

#[tokio::test]
async fn a_malformed_notification_is_not_passed_on_and_the_owner_is_told() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    scene
        .adapter
        .notify("selectionChanged", json!({"filePath": "/a", "text": 1}))
        .await;
    scene
        .adapter
        .notify("atMention", json!({"filePath": "/a"}))
        .await;

    for expected_path in ["$.text", "$.lineStart"] {
        match timeout(WAIT, scene.workspace.next_event()).await.unwrap() {
            Some(WorkspaceEvent::MalformedNotification { error, .. }) => {
                assert_eq!(error.path, expected_path);
            }
            other => panic!("expected a malformed notification, got {other:?}"),
        }
        // The notification itself still reaches the owner.
        assert!(matches!(
            timeout(WAIT, scene.workspace.next_event()).await.unwrap(),
            Some(WorkspaceEvent::Notification { .. })
        ));
    }
    // Nothing went to Claude Code: the next message that it gets is for the request that it makes now.
    let answer = scene.claude.request("ping", json!({})).await;
    assert_eq!(answer["result"], json!({}));
}

#[tokio::test]
async fn a_claude_code_that_connects_later_is_told_the_selection_that_is_current() {
    let mut scene = scene_declaring(&READ_ONLY).await;

    // Before there is a selection, a new client is told nothing.
    let port = scene.workspace.port();
    let token = scene.fixture.read_lock(port).auth_token;
    let mut early = MockClaude::connect(port, &token).await;
    early.handshake(4322).await;
    let answer = early.request("ping", json!({})).await;
    assert_eq!(answer["result"], json!({}));

    // The selection changes twice; the second one is the current one.
    scene
        .adapter
        .notify("selectionChanged", selection("first", (0, 0), (0, 5)))
        .await;
    scene
        .adapter
        .notify("selectionChanged", selection("second", (1, 0), (1, 6)))
        .await;
    // (A selection that is replaced before it is sent is not sent; the one that is current always is.)
    loop {
        let message = scene.claude.recv_json().await.expect("a notification");
        if message["params"]["text"] == "second" {
            break;
        }
    }

    let mut late = MockClaude::connect(port, &token).await;
    late.handshake(4323).await;
    let message = late.recv_json().await.expect("the current selection");
    assert_eq!(message["method"], "selection_changed");
    assert_eq!(message["params"]["text"], "second");
}

#[tokio::test]
async fn a_selection_is_sent_to_every_connected_client() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    let port = scene.workspace.port();
    let token = scene.fixture.read_lock(port).auth_token;
    let mut second = MockClaude::connect(port, &token).await;
    second.handshake(4322).await;
    second.tool_names().await;

    scene
        .adapter
        .notify("selectionChanged", selection("x", (0, 0), (0, 1)))
        .await;
    for claude in [&mut scene.claude, &mut second] {
        let message = claude.recv_json().await.expect("a notification");
        assert_eq!(message["method"], "selection_changed");
    }
}

#[tokio::test]
async fn a_tool_that_the_adapter_did_not_declare_is_not_offered() {
    // Only getOpenEditors is declared: the other read-only tools are neither listed nor callable.
    let mut scene: Scene = scene_declaring(&["getOpenEditors"]).await;
    assert_eq!(
        scene.claude.tool_names().await,
        ["getOpenEditors", "getWorkspaceFolders"]
    );
    let answer = scene
        .claude
        .request(
            "tools/call",
            json!({"name": "getCurrentSelection", "arguments": {}}),
        )
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
async fn a_file_path_that_is_not_absolute_is_not_passed_on() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    let mut relative = selection("x", (0, 0), (0, 1));
    relative["filePath"] = json!("src/a.lua");
    scene.adapter.notify("selectionChanged", relative).await;
    match timeout(WAIT, scene.workspace.next_event()).await.unwrap() {
        Some(WorkspaceEvent::MalformedNotification { error, .. }) => {
            assert_eq!(error.path, "$.filePath");
            assert!(error.reason.contains("absolute"), "{error}");
        }
        other => panic!("expected a malformed notification, got {other:?}"),
    }
    let answer = scene.claude.request("ping", json!({})).await;
    assert_eq!(answer["result"], json!({}));
}

/// The owner of the workspace does not have to read the events: Claude Code still gets every selection, and the
/// workspace goes on serving (tool calls, new clients).
#[tokio::test]
async fn an_owner_that_does_not_read_the_events_does_not_stall_the_workspace() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    // More than the queue of events holds, and nobody calls next_event().
    for index in 0..600 {
        scene
            .adapter
            .notify(
                "selectionChanged",
                selection(&format!("s{index}"), (index, 0), (index, 1)),
            )
            .await;
    }
    // Claude Code reads only now. A selection is a state: it has been given the current one, and the ones that it was
    // too slow for were replaced, not queued (and the last one is never the one that was lost).
    let mut last = Value::Null;
    let mut received = 0;
    while last["params"]["text"] != "s599" {
        last = scene.claude.recv_json().await.expect("a notification");
        received += 1;
    }
    assert!(received <= 600);
    assert!(
        scene.workspace.dropped_events() > 0,
        "the events that were not read were dropped, not waited for"
    );

    // The workspace still answers.
    let (answer, _) = call_tool(
        &mut scene,
        "getOpenEditors",
        json!({}),
        Ok(json!({"tabs": []})),
    )
    .await;
    assert_eq!(answer["result"]["isError"], false);
}

/// A Claude Code that does not read what it is sent must not hold up the others.
#[tokio::test]
async fn a_client_that_does_not_keep_up_does_not_hold_up_the_others() {
    let mut scene = scene_declaring(&READ_ONLY).await;
    let port = scene.workspace.port();
    let token = scene.fixture.read_lock(port).auth_token;
    // Connects, completes the handshake, and then never reads again.
    let mut stalled = MockClaude::connect(port, &token).await;
    stalled.handshake(4399).await;
    stalled.tool_names().await;

    // Large selections, many of them: far more than the buffers between here and the stalled client can hold.
    let big = "x".repeat(8 * 1024);
    let count = 2000;
    for index in 0..count {
        scene
            .adapter
            .notify("selectionChanged", selection(&big, (index, 0), (index, 1)))
            .await;
        // The healthy client reads as it goes.
        let message = scene.claude.recv_json().await.expect("a notification");
        assert_eq!(
            message["method"], "selection_changed",
            "notification {index}"
        );
    }
    // And the workspace still answers.
    let (answer, _) = call_tool(
        &mut scene,
        "getOpenEditors",
        json!({}),
        Ok(json!({"tabs": []})),
    )
    .await;
    assert_eq!(answer["result"]["isError"], false);
    drop(stalled);
}
