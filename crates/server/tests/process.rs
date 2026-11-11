//! The executable, as a child process: what a user, or a supervisor, starts.

// The adapter end and the stand-in for Claude Code are those of the core's own tests.
#[path = "../../core/tests/common/mod.rs"]
mod common;

use std::os::unix::net::UnixListener as StdUnixListener;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use claude_ide_core::lock::LockContents;
use common::claude::MockClaude;
use common::{Client, registration};
use serde_json::json;
use tempfile::TempDir;
use tokio::net::UnixStream;
use tokio::time::timeout;

const WAIT: Duration = Duration::from_secs(10);

/// The directories that a server started by a test works in: all of them under one temporary directory.
struct Home {
    dir: TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: TempDir::new().unwrap(),
        }
    }

    fn socket(&self) -> PathBuf {
        self.dir.path().join("run").join("adapter.sock")
    }

    fn lock_dir(&self) -> PathBuf {
        self.dir.path().join("claude").join("ide")
    }

    /// The server, with its environment pointed at this directory and its log in a file.
    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_claude-ide-server"));
        command
            .env("CLAUDE_CONFIG_DIR", self.dir.path().join("claude"))
            .env("XDG_RUNTIME_DIR", self.dir.path().join("xdg"))
            .env("HOME", self.dir.path())
            .env_remove("LISTEN_FDS")
            .env_remove("LISTEN_PID")
            .env_remove("RUST_LOG")
            .env_remove("CLAUDE_IDE_SERVER_LOG")
            .stdin(Stdio::null())
            .stderr(Stdio::from(std::fs::File::create(self.log_path()).unwrap()));
        command
    }

    fn log_path(&self) -> PathBuf {
        self.dir.path().join("server.log")
    }

    fn log(&self) -> String {
        std::fs::read_to_string(self.log_path()).unwrap_or_default()
    }

    fn lock_files(&self) -> Vec<PathBuf> {
        match std::fs::read_dir(self.lock_dir()) {
            Ok(entries) => entries
                .map(|entry| entry.unwrap().path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "lock"))
                .collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// A child process that is killed if a test fails before it is waited for.
struct Server(Child);

impl Server {
    fn terminate(&self) {
        let status = Command::new("kill")
            .arg("-TERM")
            .arg(self.0.id().to_string())
            .status()
            .unwrap();
        assert!(status.success());
    }

    async fn wait(&mut self) -> ExitStatus {
        timeout(WAIT, async {
            loop {
                if let Some(status) = self.0.try_wait().unwrap() {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the server ends")
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn connect_when_ready(socket: &Path) -> UnixStream {
    timeout(WAIT, async {
        loop {
            if let Ok(stream) = UnixStream::connect(socket).await {
                return stream;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the socket is there")
}

/// What an adapter does: registers, and gets the port of the workspace.
async fn register(socket: &Path) -> (Client<UnixStream>, u16) {
    let mut adapter = Client::new(connect_when_ready(socket).await);
    let port = adapter.register(registration()).await.unwrap();
    (adapter, port)
}

async fn wait_for_log(home: &Home, text: &str) {
    timeout(WAIT, async {
        while !home.log().contains(text) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the log says {text:?}:\n{}", home.log()));
}

fn token_of(lock_files: &[PathBuf], port: u16) -> String {
    let path = lock_files
        .iter()
        .find(|path| {
            path.file_stem()
                .is_some_and(|stem| stem.to_string_lossy() == port.to_string())
        })
        .expect("the lock file of the port");
    let contents: LockContents =
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    contents.auth_token
}

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

#[tokio::test]
async fn a_socket_that_is_handed_over_is_served_and_stays_when_the_server_goes() {
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
