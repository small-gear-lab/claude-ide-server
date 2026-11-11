//! The delivery of notifications to Claude Code, on its own: a hub over a WebSocket server, with no workspace and no
//! adapter. What matters here is that a client that does not read cannot hold up the others.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use claude_ide_core::mcp::{
    ClientChange, McpHub, ToolBackend, ToolDefinition, ToolError, ToolResult,
};
use claude_ide_core::ws::{self, WsServer};
use common::claude::MockClaude;
use serde_json::{Value, json};
use tokio::task::JoinHandle;
use tokio::time::timeout;

const TOKEN: &str = "0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(5);

/// A hub that has no tools: only the delivery of notifications is of interest here.
struct NoTools;

impl ToolBackend for NoTools {
    fn tools(&self) -> Vec<ToolDefinition> {
        Vec::new()
    }

    async fn call(&self, name: &str, _arguments: Value) -> Result<ToolResult, ToolError> {
        Err(ToolError::UnknownTool(name.to_owned()))
    }
}

struct Rig {
    hub: McpHub<NoTools>,
    changes: Arc<Mutex<Vec<ClientChange>>>,
    port: u16,
    accepting: JoinHandle<()>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self.accepting.abort();
    }
}

async fn rig() -> Rig {
    let mut server = WsServer::start(TOKEN, ws::Config::default()).await.unwrap();
    let port = server.port();
    let changes = Arc::new(Mutex::new(Vec::new()));
    let recording = changes.clone();
    let hub = McpHub::new(
        NoTools,
        "test",
        "0",
        WAIT,
        Arc::new(move |change| recording.lock().unwrap().push(change)),
    );
    let attaching = hub.clone();
    let accepting = tokio::spawn(async move {
        while let Some(connection) = server.accept().await {
            attaching.attach(connection);
        }
        drop(server);
    });
    Rig {
        hub,
        changes,
        port,
        accepting,
    }
}

impl Rig {
    /// A client that has completed the handshake and is registered with the hub.
    async fn client(&self) -> MockClaude {
        let expected = self.hub.clients().len() + 1;
        let mut claude = MockClaude::connect(self.port, TOKEN).await;
        claude.handshake(4000 + expected as u32).await;
        self.wait_for_clients(expected).await;
        claude
    }

    async fn wait_for_clients(&self, count: usize) {
        timeout(WAIT, async {
            while self.hub.clients().len() != count {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "{count} clients are registered (now {})",
                self.hub.clients().len()
            )
        });
    }
}

fn number(message: &Value) -> u64 {
    message["params"]["n"].as_u64().unwrap()
}

/// A notification with a lot in it, so that a client that does not read soon fills every buffer on the way.
fn bulky(n: u64) -> Value {
    json!({ "n": n, "padding": "x".repeat(8 * 1024) })
}

#[tokio::test]
async fn notifications_reach_every_client_in_order() {
    let rig = rig().await;
    let mut first = rig.client().await;
    let mut second = rig.client().await;

    for n in 0..10 {
        rig.hub.notify("event", json!({ "n": n }));
    }
    for claude in [&mut first, &mut second] {
        for n in 0..10 {
            let message = claude.recv_json().await.expect("a notification");
            assert_eq!(message["method"], "event");
            assert_eq!(number(&message), n);
        }
    }
}

#[tokio::test]
async fn a_client_that_does_not_read_does_not_hold_up_the_others() {
    let rig = rig().await;
    let _stalled = rig.client().await; // never reads again
    let mut healthy = rig.client().await;

    // Far more than the buffers between here and the stalled client can hold.
    for n in 0..1500 {
        rig.hub.notify("event", bulky(n));
        let message = healthy.recv_json().await.expect("a notification");
        assert_eq!(number(&message), n);
    }
}

#[tokio::test]
async fn a_state_replaces_the_one_that_is_waiting_and_the_latest_is_never_lost() {
    let rig = rig().await;
    let mut slow = rig.client().await;
    let mut healthy = rig.client().await;

    for n in 0..1500 {
        rig.hub.notify_remembering("state", bulky(n));
        // The healthy client reads as it goes and gets every one.
        assert_eq!(number(&healthy.recv_json().await.unwrap()), n);
    }

    // The slow client starts to read only now: it has the states that fitted in the buffers, then the current one.
    let mut previous = None;
    let mut received = 0;
    loop {
        let n = number(&slow.recv_json().await.expect("a notification"));
        assert!(previous.is_none_or(|before| before < n), "in order");
        previous = Some(n);
        received += 1;
        if n == 1499 {
            break;
        }
    }
    assert!(received <= 1500);
}

#[tokio::test]
async fn events_are_kept_in_order_and_the_oldest_is_dropped_when_too_many_wait() {
    let rig = rig().await;
    let mut slow = rig.client().await;
    let mut healthy = rig.client().await;

    for n in 0..1500 {
        rig.hub.notify("event", bulky(n));
        assert_eq!(number(&healthy.recv_json().await.unwrap()), n);
    }

    let mut previous = None;
    loop {
        let n = number(&slow.recv_json().await.expect("a notification"));
        assert!(
            previous.is_none_or(|before| before < n),
            "in order, without repeats"
        );
        previous = Some(n);
        if n == 1499 {
            break; // the newest are kept
        }
    }
}

#[tokio::test]
async fn a_client_that_connects_later_is_sent_the_state_that_is_current_and_no_history() {
    let rig = rig().await;
    rig.hub.notify("event", json!({ "n": 0 }));
    rig.hub.notify_remembering("state", json!({ "n": 1 }));
    rig.hub.notify_remembering("state", json!({ "n": 2 }));

    let mut late = rig.client().await;
    let message = late.recv_json().await.expect("the current state");
    assert_eq!(message["method"], "state");
    assert_eq!(number(&message), 2);

    // Nothing else was waiting for it: the next thing is what is sent now.
    rig.hub.notify("event", json!({ "n": 3 }));
    let message = late.recv_json().await.expect("a notification");
    assert_eq!(message["method"], "event");
    assert_eq!(number(&message), 3);
}

#[tokio::test]
async fn a_client_that_leaves_is_forgotten_and_sending_to_nobody_is_harmless() {
    let rig = rig().await;
    let claude = rig.client().await;
    assert_eq!(rig.hub.clients().len(), 1);
    drop(claude);
    rig.wait_for_clients(0).await;
    rig.hub.notify("event", json!({ "n": 0 }));
    rig.hub.notify_remembering("state", json!({ "n": 0 }));
}

#[tokio::test]
async fn the_observer_is_told_when_a_client_connects_and_when_it_goes() {
    let rig = rig().await;
    let claude = rig.client().await;
    {
        let changes = rig.changes.lock().unwrap();
        assert_eq!(changes.len(), 1);
        assert!(matches!(&changes[0], ClientChange::Connected(info) if info.name == "claude-code"));
    }

    drop(claude);
    timeout(WAIT, async {
        while rig.changes.lock().unwrap().len() < 2 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the disconnection is observed");
    let changes = rig.changes.lock().unwrap();
    // What was known of it, the process included, comes with the disconnection.
    assert!(matches!(&changes[1], ClientChange::Disconnected(info) if info.pid.is_some()));
}

#[tokio::test]
async fn an_observer_may_ask_the_hub_about_its_clients() {
    use std::sync::OnceLock;

    let mut server = WsServer::start(TOKEN, ws::Config::default()).await.unwrap();
    let port = server.port();
    let hub_cell: Arc<OnceLock<McpHub<NoTools>>> = Arc::new(OnceLock::new());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (cell, record) = (hub_cell.clone(), seen.clone());
    let hub = McpHub::new(
        NoTools,
        "test",
        "0",
        WAIT,
        Arc::new(move |change| {
            // Called with the hub's own lock of its clients not held: this would otherwise wait for itself.
            let count = cell.get().map_or(0, |hub| hub.clients().len());
            record.lock().unwrap().push((change, count));
        }),
    );
    hub_cell.set(hub.clone()).ok();
    let accepting = tokio::spawn(async move {
        while let Some(connection) = server.accept().await {
            hub.attach(connection);
        }
    });

    let mut claude = MockClaude::connect(port, TOKEN).await;
    claude.handshake(4000).await;
    timeout(WAIT, async {
        while seen.lock().unwrap().is_empty() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the observer is told, and does not wait for itself");
    assert_eq!(seen.lock().unwrap()[0].1, 1);
    accepting.abort();
}
