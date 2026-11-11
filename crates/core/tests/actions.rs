//! The tools that act on a file: `openFile`, `checkDocumentDirty` and `saveDocument`. What an adapter answers is checked and
//! turned into the text that the VS Code extension writes; a path that is not absolute is taken from the first folder of
//! the workspace before the adapter is asked.

use claude_ide_test_support as common;

use claude_ide_core::jsonrpc::RpcError;
use common::scene::{call_tool, result_json, scene_declaring};
use serde_json::{Value, json};

const ACTIONS: [&str; 3] = ["openFile", "checkDocumentDirty", "saveDocument"];

/// What an adapter answers to `openFile`.
fn opened(extra: Value) -> Value {
    let mut answer = json!({
        "filePath": "/home/user/project/lib/a.lua",
        "languageId": "lua",
        "lineCount": 12,
        "isDirty": false,
        "isUntitled": false,
    });
    for (key, value) in extra.as_object().unwrap() {
        answer[key] = value.clone();
    }
    answer
}

fn text_of(answer: &Value) -> &str {
    answer["result"]["content"][0]["text"].as_str().unwrap()
}

#[tokio::test]
async fn a_relative_path_is_made_absolute_before_the_adapter_is_asked() {
    let mut scene = scene_declaring(&ACTIONS).await;
    // The registration of the scene has one folder: /home/user/project.
    let (answer, (method, params)) = call_tool(
        &mut scene,
        "openFile",
        json!({"filePath": "lib/../lib/a.lua"}),
        Ok(opened(json!({}))),
    )
    .await;
    assert_eq!(method, "openFile");
    assert_eq!(params["filePath"], "/home/user/project/lib/a.lua");
    assert_eq!(
        text_of(&answer),
        "Opened file: /home/user/project/lib/a.lua"
    );
}

#[tokio::test]
async fn open_file_answers_with_the_details_when_it_is_not_made_the_frontmost() {
    let mut scene = scene_declaring(&ACTIONS).await;
    let (answer, (_, params)) = call_tool(
        &mut scene,
        "openFile",
        json!({"filePath": "/home/user/project/lib/a.lua", "makeFrontmost": false}),
        Ok(opened(json!({}))),
    )
    .await;
    assert_eq!(params["makeFrontmost"], false);
    let details = result_json(&answer);
    assert_eq!(details["success"], true);
    assert_eq!(details["fileUrl"], "file:///home/user/project/lib/a.lua");
    assert_eq!(details["languageId"], "lua");
    assert_eq!(details["lineCount"], 12);
    assert_eq!(details["isClosed"], false);
}

#[tokio::test]
async fn open_file_says_what_came_of_the_text_to_select() {
    let mut scene = scene_declaring(&ACTIONS).await;
    for (selection, expected) in [
        (
            "range",
            "Opened file and selected text from \"foo\" to \"bar\"",
        ),
        (
            "positioned",
            "Opened file and positioned at \"foo\" (end text \"bar\" not found)",
        ),
        ("not_found", "Opened file, but text \"foo\" not found"),
    ] {
        let (answer, _) = call_tool(
            &mut scene,
            "openFile",
            json!({"filePath": "/home/user/project/lib/a.lua", "startText": "foo", "endText": "bar"}),
            Ok(opened(json!({ "selection": selection }))),
        )
        .await;
        assert_eq!(text_of(&answer), expected);
    }
    let (answer, _) = call_tool(
        &mut scene,
        "openFile",
        json!({"filePath": "/home/user/project/lib/a.lua", "startText": "foo"}),
        Ok(opened(json!({ "selection": "text" }))),
    )
    .await;
    assert_eq!(text_of(&answer), "Opened file and selected text \"foo\"");
}

#[tokio::test]
async fn an_answer_to_open_file_that_is_not_valid_fails_the_call_and_says_where() {
    let mut scene = scene_declaring(&ACTIONS).await;
    // `selection` is asked for when `startText` is given, and is not there.
    let (answer, _) = call_tool(
        &mut scene,
        "openFile",
        json!({"filePath": "/home/user/project/lib/a.lua", "startText": "foo"}),
        Ok(opened(json!({}))),
    )
    .await;
    assert_eq!(answer["result"]["isError"], true);
    assert!(text_of(&answer).contains("$.selection"), "{answer}");

    let mut incomplete = opened(json!({}));
    incomplete.as_object_mut().unwrap().remove("lineCount");
    let (answer, _) = call_tool(
        &mut scene,
        "openFile",
        json!({"filePath": "/home/user/project/lib/a.lua"}),
        Ok(incomplete),
    )
    .await;
    assert_eq!(answer["result"]["isError"], true);
    assert!(text_of(&answer).contains("$.lineCount"), "{answer}");
}

#[tokio::test]
async fn a_file_that_cannot_be_opened_is_a_failed_call_with_the_reason() {
    let mut scene = scene_declaring(&ACTIONS).await;
    let (answer, _) = call_tool(
        &mut scene,
        "openFile",
        json!({"filePath": "/nowhere.lua"}),
        Err(RpcError::new(-1, "File not found: /nowhere.lua")),
    )
    .await;
    assert_eq!(answer["result"]["isError"], true);
    assert_eq!(text_of(&answer), "File not found: /nowhere.lua");
}

#[tokio::test]
async fn the_state_of_a_document_is_told_and_one_that_is_not_open_is_said_so() {
    let mut scene = scene_declaring(&ACTIONS).await;
    let (answer, (method, params)) = call_tool(
        &mut scene,
        "checkDocumentDirty",
        json!({"filePath": "lib/a.lua"}),
        Ok(json!({"isDirty": true, "isUntitled": false})),
    )
    .await;
    assert_eq!(method, "checkDocumentDirty");
    assert_eq!(params["filePath"], "/home/user/project/lib/a.lua");
    assert_eq!(
        result_json(&answer),
        json!({"success": true, "filePath": "/home/user/project/lib/a.lua", "isDirty": true, "isUntitled": false})
    );

    let (answer, _) = call_tool(
        &mut scene,
        "checkDocumentDirty",
        json!({"filePath": "/home/user/project/other.lua"}),
        Ok(json!({"success": false})),
    )
    .await;
    assert_eq!(
        answer["result"]["isError"], false,
        "it is an answer, not a failure"
    );
    assert_eq!(
        result_json(&answer),
        json!({"success": false, "message": "Document not open: /home/user/project/other.lua"})
    );
}

#[tokio::test]
async fn a_document_is_saved_and_the_ways_that_it_may_not_be() {
    let mut scene = scene_declaring(&ACTIONS).await;
    let (answer, (method, _)) = call_tool(
        &mut scene,
        "saveDocument",
        json!({"filePath": "/home/user/project/a.lua"}),
        Ok(json!({"saved": true})),
    )
    .await;
    assert_eq!(method, "saveDocument");
    assert_eq!(
        result_json(&answer),
        json!({"success": true, "filePath": "/home/user/project/a.lua", "saved": true, "message": "Document saved successfully"})
    );

    let (answer, _) = call_tool(
        &mut scene,
        "saveDocument",
        json!({"filePath": "/home/user/project/a.lua"}),
        Ok(json!({"saved": false})),
    )
    .await;
    let not_saved = result_json(&answer);
    assert_eq!(not_saved["saved"], false);
    assert_eq!(
        not_saved["message"],
        "Document was not dirty or save failed"
    );

    let (answer, _) = call_tool(
        &mut scene,
        "saveDocument",
        json!({"filePath": "/home/user/project/a.lua"}),
        Ok(json!({"success": false})),
    )
    .await;
    assert_eq!(
        result_json(&answer)["message"],
        "Document not open: /home/user/project/a.lua"
    );

    // A required argument that is missing fails the call, and the adapter is not asked.
    let call = scene.claude.request(
        "tools/call",
        json!({"name": "saveDocument", "arguments": {}}),
    );
    let answer = call.await;
    assert_eq!(answer["result"]["isError"], true);
    assert!(text_of(&answer).contains("filePath is required"));
}
