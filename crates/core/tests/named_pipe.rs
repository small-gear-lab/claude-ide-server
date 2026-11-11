//! The adapter side over a real named pipe (Windows): that one server has a name at a time, that clients that come one
//! after another, or together, each get an instance, and that the name is free when the server is gone. On Unix the same
//! is asked of a Unix socket, in `unix_socket.rs`.

#![cfg(windows)]

mod common;

use std::io;
use std::path::Path;
use std::time::Duration;

use claude_ide_core::adapter::{AdapterServer, Config, PipeListener};
use common::{Client, WAIT, connect_to_adapters, registration};
use tokio::time::timeout;

const PORT: u16 = 4000;

/// A name that no other test, and no other run, has.
fn pipe_name(test: &str) -> String {
    format!(
        r"\\.\pipe\claude-ide-core-test-{}-{test}",
        std::process::id()
    )
}

#[tokio::test]
async fn it_works_over_a_real_named_pipe_that_is_for_one_server_at_a_time() {
    let name = pipe_name("one-server");
    let listener = PipeListener::bind(&name).unwrap();
    assert_eq!(listener.path().unwrap().to_string_lossy(), name);
    // Another server on the same name is refused while this one is alive.
    assert_eq!(
        PipeListener::bind(&name).unwrap_err().kind(),
        io::ErrorKind::AddrInUse
    );

    let mut server = AdapterServer::start(listener, Config::default());
    let mut client = Client::new(connect_to_adapters(Path::new(&name)).await.unwrap());
    client.request(1, "register", registration()).await;
    let registering = timeout(WAIT, server.accept()).await.unwrap().unwrap();
    let _session = registering.accept(PORT);
    assert_eq!(client.registered().await.unwrap(), PORT);

    // A pipe goes with its last handle: once the server is gone the name is free.
    drop(_session);
    drop(server);
    timeout(WAIT, async {
        loop {
            if let Ok(listener) = PipeListener::bind(&name) {
                return listener;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the name is free again");
}

#[tokio::test]
async fn adapters_that_come_one_after_another_or_together_each_get_an_instance() {
    let name = pipe_name("instances");
    let mut server = AdapterServer::start(PipeListener::bind(&name).unwrap(), Config::default());

    // Together: all of them connect before any registers.
    let mut clients = Vec::new();
    for index in 0..4u32 {
        let mut client = Client::new(connect_to_adapters(Path::new(&name)).await.unwrap());
        let mut registration = registration();
        registration["pid"] = (2000 + index).into();
        client.request(1, "register", registration).await;
        clients.push(client);
    }
    let mut sessions = Vec::new();
    for index in 0..4u32 {
        let registering = timeout(WAIT, server.accept()).await.unwrap().unwrap();
        // The order of the registrations is the order of the connections.
        assert_eq!(registering.registration().pid, 2000 + index);
        sessions.push(registering.accept(PORT + index as u16));
    }
    for (index, client) in clients.iter_mut().enumerate() {
        assert_eq!(client.registered().await.unwrap(), PORT + index as u16);
    }

    // One after another: a client that has gone does not use up an instance.
    drop(clients);
    drop(sessions);
    for _ in 0..3 {
        let mut client = Client::new(connect_to_adapters(Path::new(&name)).await.unwrap());
        client.request(1, "register", registration()).await;
        let registering = timeout(WAIT, server.accept()).await.unwrap().unwrap();
        let _session = registering.accept(PORT);
        assert_eq!(client.registered().await.unwrap(), PORT);
    }
}
