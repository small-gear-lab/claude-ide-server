//! The executable, as a child process: what a user, or a supervisor, starts.

// The adapter end and the stand-in for Claude Code are those of the core's own tests.
#[path = "../../core/tests/common/mod.rs"]
mod common;
mod support;

use std::path::Path;
#[cfg(unix)]
use std::process::Command;
use std::process::Stdio;

use common::claude::MockClaude;
use common::{Client, registration};
use serde_json::json;
#[cfg(unix)]
use support::wait_for_log;
use support::{Home, Server, connect_when_ready, token_of};

/// What an adapter does: registers, and gets the port of the workspace.
async fn register(socket: &Path) -> (Client<common::AdapterStream>, u16) {
    let mut adapter = Client::new(connect_when_ready(socket).await);
    let port = adapter.register(registration()).await.unwrap();
    (adapter, port)
}

#[cfg(unix)]
#[tokio::test]
async fn it_serves_an_adapter_and_a_claude_code_and_leaves_nothing_when_terminated() {
    let home = Home::new();
    let mut server = Server(
        home.command()
            .arg("--socket")
            .arg(home.socket())
            .spawn()
            .unwrap(),
    );

    let (mut adapter, port) = register(&home.socket()).await;
    let token = token_of(&home.lock_files(), port);
    let mut claude = MockClaude::connect(port, &token).await;
    claude.handshake(4242).await;
    // A tool that the server answers by itself.
    let answer = claude
        .request(
            "tools/call",
            json!({"name": "getWorkspaceFolders", "arguments": {}}),
        )
        .await;
    assert!(
        answer.to_string().contains("/home/user/project"),
        "{answer}"
    );

    // A Claude Code that goes is logged, with its process if it said which.
    drop(claude);
    wait_for_log(&home, "is gone from the workspace of Neovim (pid 1234)").await;
    assert!(home.log().contains("(pid 4242)"), "{}", home.log());

    server.terminate();
    let status = server.wait().await;
    assert_eq!(status.code(), Some(0), "{}", home.log());
    assert!(home.lock_files().is_empty());
    assert!(!home.socket().exists());
    // The adapter was told to shut down (its stream ends).
    while adapter.recv().await.is_some() {}

    let log = home.log();
    assert!(log.contains("listening for adapters on"), "{log}");
    assert!(log.contains("adapter registered: Neovim"), "{log}");
    assert!(
        log.contains("connected to the workspace of Neovim (pid 1234)"),
        "{log}"
    );
    assert!(
        log.contains("is gone from the workspace of Neovim"),
        "{log}"
    );
    assert!(log.contains("stopped"), "{log}");
}

/// Windows has no `SIGTERM` for a service to be asked with: a task that is ended, like a process that is killed, gets no
/// chance to clean up. The lock file is deleted all the same, when the system closes the handle that the server kept on it.
#[cfg(windows)]
#[tokio::test]
async fn a_server_that_is_killed_leaves_no_lock_file_behind() {
    let home = Home::new();
    let mut server = Server(
        home.command()
            .arg("--socket")
            .arg(home.socket())
            .spawn()
            .unwrap(),
    );

    let (_adapter, port) = register(&home.socket()).await;
    let token = token_of(&home.lock_files(), port);
    let mut claude = MockClaude::connect(port, &token).await;
    claude.handshake(4242).await;
    let answer = claude
        .request(
            "tools/call",
            json!({"name": "getWorkspaceFolders", "arguments": {}}),
        )
        .await;
    assert!(
        answer.to_string().contains("/home/user/project"),
        "{answer}"
    );
    assert_eq!(home.lock_files().len(), 1);

    server.kill();
    server.wait().await;
    drop(claude);
    // The system closes the handles of a process that has ended, and the files that were to go with them go.
    tokio::time::timeout(support::WAIT, async {
        while !home.lock_files().is_empty() {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the lock file is deleted");
}

#[tokio::test]
async fn a_second_server_on_the_same_socket_fails_with_a_message() {
    let home = Home::new();
    let first = Server(
        home.command()
            .arg("--socket")
            .arg(home.socket())
            .spawn()
            .unwrap(),
    );
    let _ = connect_when_ready(&home.socket()).await;

    let output = home
        .command()
        .arg("--socket")
        .arg(home.socket())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(message.contains("another server is listening"), "{message}");
    drop(first);
}

#[cfg(unix)]
#[tokio::test]
async fn a_socket_that_is_handed_over_is_served_and_stays_when_the_server_goes() {
    use std::os::unix::net::UnixListener as StdUnixListener;

    let home = Home::new();
    std::fs::create_dir_all(home.socket().parent().unwrap()).unwrap();
    let handed = StdUnixListener::bind(home.socket()).unwrap();
    // What a supervisor does: the socket is file descriptor 3 of the process, and LISTEN_PID is that process. The
    // shell makes that so (it is the same process after `exec`), and stands for the supervisor here.
    let mut command = Command::new("sh");
    command
        .arg("-c")
        .arg(r#"LISTEN_FDS=1 LISTEN_PID=$$ exec "$0" "$@" 3<&0 </dev/null"#)
        .arg(env!("CARGO_BIN_EXE_claude-ide-server"))
        .arg("--idle-timeout")
        .arg("1")
        .env("CLAUDE_CONFIG_DIR", home.dir.path().join("claude"))
        .env("XDG_RUNTIME_DIR", home.dir.path().join("xdg"))
        .env("HOME", home.dir.path())
        .stdin(Stdio::from(std::os::fd::OwnedFd::from(handed)))
        .stderr(Stdio::from(std::fs::File::create(home.log_path()).unwrap()));
    let mut server = Server(command.spawn().unwrap());

    // The server is not listening on a socket of its own: the one that was handed over is served.
    let (adapter, _port) = register(&home.socket()).await;
    drop(adapter);
    // With no adapter for a second, it ends by itself.
    let status = server.wait().await;
    assert_eq!(status.code(), Some(0), "{}", home.log());
    // The file is the supervisor's: it stays.
    assert!(home.socket().exists());
    let log = home.log();
    assert!(log.contains("handed over"), "{log}");
    assert!(log.contains("no adapter for 1 seconds"), "{log}");
}

#[tokio::test]
async fn it_ends_by_itself_when_no_adapter_ever_comes_if_told_to() {
    let home = Home::new();
    let mut server = Server(
        home.command()
            .arg("--socket")
            .arg(home.socket())
            .args(["--idle-timeout", "1"])
            .spawn()
            .unwrap(),
    );
    assert_eq!(server.wait().await.code(), Some(0), "{}", home.log());
    assert!(!home.socket().exists());
}

/// For an adapter that cannot work out the default as the server does (see `--help`).
#[test]
fn the_socket_is_printed_for_an_adapter_that_has_to_know_it() {
    let home = Home::new();
    let print = |extra: &[&str]| {
        let output = home
            .command()
            .arg("--print-socket")
            .args(extra)
            .stderr(Stdio::piped())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(0));
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    };
    // What was asked for, as it was asked for.
    assert_eq!(
        print(&["--socket", &home.socket().to_string_lossy()]),
        home.socket().to_string_lossy()
    );
    // The environment, when it says (a test on Windows has it set, for the pipe of the test).
    assert_eq!(
        home.command()
            .env("CLAUDE_IDE_SERVER_SOCKET", "from-the-environment")
            .arg("--print-socket")
            .output()
            .map(|output| String::from_utf8(output.stdout).unwrap().trim().to_owned())
            .unwrap(),
        "from-the-environment"
    );
    // And the switch wins over it.
    assert_eq!(
        home.command()
            .env("CLAUDE_IDE_SERVER_SOCKET", "from-the-environment")
            .args(["--print-socket", "--socket", "from-the-switch"])
            .output()
            .map(|output| String::from_utf8(output.stdout).unwrap().trim().to_owned())
            .unwrap(),
        "from-the-switch"
    );
    // Otherwise the default of this system.
    let default = home
        .command()
        .env_remove("CLAUDE_IDE_SERVER_SOCKET")
        .arg("--print-socket")
        .output()
        .map(|output| String::from_utf8(output.stdout).unwrap().trim().to_owned())
        .unwrap();
    #[cfg(unix)]
    assert_eq!(default, home.default_socket().to_string_lossy());
    #[cfg(windows)]
    assert!(
        default.starts_with(r"\\.\pipe\claude-ide-server-S-1-"),
        "{default}"
    );
    // And the server is not started: nothing listens on what was printed.
    assert!(home.lock_files().is_empty());
}

#[test]
fn a_wrong_command_line_is_told_and_exits_with_2() {
    let home = Home::new();
    let output = home
        .command()
        .arg("--nope")
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unknown argument"));
}
