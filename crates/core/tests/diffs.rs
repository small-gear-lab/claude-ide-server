//! `openDiff`, `close_tab` and `closeAllDiffTabs`: a diff waits for the user, nothing else waits for it, it ends with the
//! contents of the file or with a rejection, and it does not stay when the one who asked for it has gone.

use claude_ide_test_support as common;

use std::time::Duration;

use claude_ide_core::jsonrpc::Message;
use common::Client;
use common::claude::MockClaude;
use common::fixture::Fixture;
use common::scene::{Scene, scene_declaring, scene_in, scene_with};
use serde_json::{Value, json};
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(5);

const DIFFS: [&str; 3] = ["openDiff", "closeDiff", "closeAllDiffs"];

fn open_diff_arguments(tab_name: &str) -> Value {
    json!({
        "old_file_path": "/home/user/project/a.lua",
        "new_file_path": "/home/user/project/a.lua",
        "new_file_contents": "return 2\n",
        "tab_name": tab_name,
    })
}

/// A Claude Code that has connected to the workspace of `scene`, besides the one that the scene has.
async fn another_claude(scene: &Scene) -> MockClaude {
    let port = scene.workspace.port();
    let token = scene.fixture.read_lock(port).auth_token;
    let mut claude = MockClaude::connect(port, &token).await;
    claude.handshake(4322).await;
    claude.tool_names().await;
    claude
}

/// `tools/call` in a task of its own, since it does not end until the adapter answers.
fn call_in_task(
    mut claude: MockClaude,
    name: &str,
    arguments: Value,
) -> JoinHandle<(MockClaude, Value)> {
    let name = name.to_owned();
    tokio::spawn(async move {
        let answer = claude
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        (claude, answer)
    })
}

/// The next request that the adapter gets: its number, its method and its parameters.
async fn next_request(adapter: &mut Client<DuplexStream>) -> (Value, String, Value) {
    match timeout(WAIT, adapter.recv())
        .await
        .expect("a request arrives")
    {
        Some(Message::Request { id, method, params }) => (id, method, params),
        other => panic!("expected a request, got {other:?}"),
    }
}

async fn answer(adapter: &mut Client<DuplexStream>, id: Value, outcome: Value) {
    adapter
        .send(Message::Response {
            id,
            outcome: Ok(outcome),
        })
        .await;
}

fn texts(answer: &Value) -> Vec<&str> {
    answer["result"]["content"]
        .as_array()
        .unwrap_or_else(|| panic!("no content in {answer}"))
        .iter()
        .map(|item| item["text"].as_str().unwrap())
        .collect()
}

#[tokio::test]
async fn an_accepted_diff_ends_with_file_saved_and_the_contents_and_nothing_waits_for_it() {
    let scene = scene_declaring(&DIFFS).await;
    let mut other = another_claude(&scene).await;
    let Scene {
        mut adapter,
        claude,
        fixture: _fixture,
        workspace: _workspace,
    } = scene;

    let call = call_in_task(claude, "openDiff", open_diff_arguments("edit-1"));
    let (id, method, params) = next_request(&mut adapter).await;
    assert_eq!(method, "openDiff");
    assert_eq!(params["oldFilePath"], "/home/user/project/a.lua");
    assert_eq!(params["newFilePath"], "/home/user/project/a.lua");
    assert_eq!(params["newFileContents"], "return 2\n");
    assert_eq!(params["tabName"], "edit-1");
    assert!(params["diffId"].as_u64().is_some(), "{params}");

    // The diff waits for the user; another Claude Code is answered at once, meanwhile.
    let folders = other
        .request(
            "tools/call",
            json!({ "name": "getWorkspaceFolders", "arguments": {} }),
        )
        .await;
    assert_eq!(folders["result"]["isError"], false, "{folders}");
    assert!(!call.is_finished());

    // The user accepts, with the contents as they are now (they may have been edited in the diff).
    answer(
        &mut adapter,
        id,
        json!({"accepted": true, "contents": "return 3 -- edited\n"}),
    )
    .await;
    let (_claude, result) = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert_eq!(texts(&result), ["FILE_SAVED", "return 3 -- edited\n"]);
}

#[tokio::test]
async fn a_rejected_diff_ends_with_diff_rejected_and_the_name_of_the_tab() {
    let scene = scene_declaring(&DIFFS).await;
    let Scene {
        mut adapter,
        claude,
        fixture: _fixture,
        workspace: _workspace,
    } = scene;
    let call = call_in_task(claude, "openDiff", open_diff_arguments("edit-2"));
    let (id, _, _) = next_request(&mut adapter).await;
    answer(&mut adapter, id, json!({"accepted": false})).await;
    let (_claude, result) = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(result["result"]["isError"], false);
    assert_eq!(texts(&result), ["DIFF_REJECTED", "edit-2"]);
}

#[tokio::test]
async fn a_diff_may_take_longer_than_the_time_that_a_request_may_take() {
    let fixture = Fixture::with_request_timeout(Duration::from_millis(300));
    let mut params = common::registration();
    params["methods"] = json!(DIFFS);
    let Scene {
        mut adapter,
        claude,
        fixture: _fixture,
        workspace: _workspace,
    } = scene_with(fixture, params).await;
    let call = call_in_task(claude, "openDiff", open_diff_arguments("slow"));
    let (id, _, _) = next_request(&mut adapter).await;
    // The user takes their time: twice what any other request may take.
    tokio::time::sleep(Duration::from_millis(700)).await;
    answer(&mut adapter, id, json!({"accepted": true, "contents": "x"})).await;
    let (_claude, result) = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(texts(&result), ["FILE_SAVED", "x"]);
}

#[tokio::test]
async fn an_answer_that_is_not_valid_fails_the_call_and_says_where() {
    let scene = scene_declaring(&DIFFS).await;
    let Scene {
        mut adapter,
        claude,
        fixture: _fixture,
        workspace: _workspace,
    } = scene;
    let call = call_in_task(claude, "openDiff", open_diff_arguments("bad"));
    let (id, _, _) = next_request(&mut adapter).await;
    // Accepted without the contents: Claude Code could not write the file.
    answer(&mut adapter, id, json!({"accepted": true})).await;
    let (_claude, result) = timeout(WAIT, call).await.unwrap().unwrap();
    assert_eq!(result["result"]["isError"], true, "{result}");
    assert!(texts(&result)[0].contains("$.contents"), "{result}");
}

#[tokio::test]
async fn a_diff_is_closed_when_the_claude_code_that_asked_for_it_goes_away() {
    let scene = scene_declaring(&DIFFS).await;
    let Scene {
        mut adapter,
        claude,
        fixture: _fixture,
        workspace: _workspace,
    } = scene;
    let call = call_in_task(claude, "openDiff", open_diff_arguments("orphan"));
    let (_, _, params) = next_request(&mut adapter).await;
    let diff_id = params["diffId"].clone();

    // Claude Code goes away (its task, and with it the connection, is dropped) while the user has not decided.
    call.abort();
    let (_, method, close) = next_request(&mut adapter).await;
    assert_eq!(method, "closeDiff");
    assert_eq!(close["tabName"], "orphan");
    assert_eq!(close["diffId"], diff_id, "it names the diff that it opened");
}

#[tokio::test]
async fn closing_the_diff_of_a_claude_code_that_is_gone_does_not_close_a_newer_one_of_the_same_name()
 {
    let scene = scene_declaring(&DIFFS).await;
    let second = another_claude(&scene).await;
    let Scene {
        mut adapter,
        claude: first,
        fixture: _fixture,
        workspace: _workspace,
    } = scene;

    let old = call_in_task(first, "openDiff", open_diff_arguments("same"));
    let (_, _, old_params) = next_request(&mut adapter).await;
    let new = call_in_task(second, "openDiff", open_diff_arguments("same"));
    let (new_id, _, new_params) = next_request(&mut adapter).await;
    assert_ne!(old_params["diffId"], new_params["diffId"]);

    // The first goes away: what is closed is the first diff, by its number, and the adapter can leave the second alone.
    old.abort();
    let (_, method, close) = next_request(&mut adapter).await;
    assert_eq!(method, "closeDiff");
    assert_eq!(close["diffId"], old_params["diffId"]);
    assert_ne!(close["diffId"], new_params["diffId"]);

    // The second goes on, and ends as the user decides.
    answer(&mut adapter, new_id, json!({"accepted": false})).await;
    let (_claude, result) = timeout(WAIT, new).await.unwrap().unwrap();
    assert_eq!(texts(&result), ["DIFF_REJECTED", "same"]);
}

#[tokio::test]
async fn closing_a_tab_is_asked_of_the_adapter_by_name_and_always_says_tab_closed() {
    let mut scene = scene_declaring(&DIFFS).await;
    let call = scene.claude.request(
        "tools/call",
        json!({ "name": "close_tab", "arguments": {"tab_name": "edit-1"} }),
    );
    let respond = async {
        let (id, method, params) = next_request(&mut scene.adapter).await;
        assert_eq!(method, "closeDiff");
        assert_eq!(
            params,
            json!({"tabName": "edit-1"}),
            "by name, without the number of a diff"
        );
        answer(&mut scene.adapter, id, json!(null)).await;
    };
    let (result, ()) = tokio::join!(call, respond);
    assert_eq!(result["result"]["isError"], false, "{result}");
    assert_eq!(texts(&result), ["TAB_CLOSED"]);
}

#[tokio::test]
async fn closing_a_tab_is_answered_even_by_an_adapter_that_shows_no_diffs() {
    let mut scene = scene_in(Fixture::new()).await;
    let result = scene
        .claude
        .request(
            "tools/call",
            json!({ "name": "close_tab", "arguments": {"tab_name": "x"} }),
        )
        .await;
    assert_eq!(texts(&result), ["TAB_CLOSED"]);
    assert!(
        timeout(Duration::from_millis(200), scene.adapter.recv())
            .await
            .is_err(),
        "the adapter was not asked"
    );
}

#[tokio::test]
async fn closing_all_diffs_says_how_many_were_closed() {
    let mut scene = scene_declaring(&DIFFS).await;
    let call = scene.claude.request(
        "tools/call",
        json!({ "name": "closeAllDiffTabs", "arguments": {} }),
    );
    let respond = async {
        let (id, method, _) = next_request(&mut scene.adapter).await;
        assert_eq!(method, "closeAllDiffs");
        answer(&mut scene.adapter, id, json!({"closed": 2})).await;
    };
    let (result, ()) = tokio::join!(call, respond);
    assert_eq!(texts(&result), ["CLOSED_2_DIFF_TABS"]);
}

#[tokio::test]
async fn a_diff_that_the_adapter_cannot_finish_does_not_leave_claude_code_waiting() {
    let scene = scene_declaring(&DIFFS).await;
    let Scene {
        mut adapter,
        mut claude,
        fixture: _fixture,
        workspace: _workspace,
    } = scene;
    claude
        .send_json(json!({
            "jsonrpc": "2.0", "id": 100, "method": "tools/call",
            "params": { "name": "openDiff", "arguments": open_diff_arguments("lost") },
        }))
        .await;
    let _ = next_request(&mut adapter).await;
    // The editor goes away while the user has not decided: the workspace ends, and with it the connection of Claude Code,
    // or the call fails; what Claude Code is not left with is a call that waits for ever.
    drop(adapter);
    match timeout(WAIT, claude.recv_json())
        .await
        .expect("it does not wait")
    {
        None => {}
        Some(answer) => assert_eq!(answer["result"]["isError"], true, "{answer}"),
    }
}
