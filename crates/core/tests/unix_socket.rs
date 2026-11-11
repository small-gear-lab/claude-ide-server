//! The adapter side over a real Unix socket: that only the user can reach, that a file that a dead server left is
//! replaced, and that a socket that something else made (a supervisor) is served and left alone. On Windows the same
//! is asked of a named pipe, in `named_pipe.rs`.

#![cfg(unix)]

mod common;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use claude_ide_core::adapter::{AdapterServer, Config, UnixListener};
use common::{Client, WAIT, registration};
use tempfile::TempDir;
use tokio::time::timeout;

const PORT: u16 = 4000;

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
    assert_eq!(listener.path(), Some(socket.as_path()));
    assert!(tokio::net::UnixStream::connect(&socket).await.is_ok());
}

#[tokio::test]
async fn a_socket_that_is_handed_over_is_served_and_is_not_removed() {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("adapter.sock");
    // What a supervisor does: makes the socket, listens, and hands it to the server when it starts.
    let handed = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    // A connection that comes before the server is there waits in the socket's queue.
    let mut early = Client::new(tokio::net::UnixStream::connect(&socket).await.unwrap());
    early.request(1, "register", registration()).await;

    let listener = UnixListener::from_listening(handed).unwrap();
    assert_eq!(listener.path(), Some(socket.as_path()));
    let mut server = AdapterServer::start(listener, Config::default());
    let registering = timeout(WAIT, server.accept()).await.unwrap().unwrap();
    let _session = registering.accept(PORT);
    assert_eq!(early.registered().await.unwrap(), PORT);

    // The server goes, and the file stays: it is the supervisor's, for the next server.
    drop(server);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(socket.exists());
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}
